//! Session-local guard for repeated patches whose final file state is unchanged.

use crate::tools::editing::{Patch, PatchLine, PatchOperation};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PatchPathState {
    pub path: PathBuf,
    pub exists: bool,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CanonicalPatchOperation {
    pub source: PathBuf,
    pub destination: Option<PathBuf>,
}

impl PatchPathState {
    /// Record one path's observed state for the no-op comparison.
    ///
    /// `content_hash` is the full-file hash the patch was planned against, so
    /// two states compare equal only when the final bytes really are unchanged.
    pub(super) fn new(path: PathBuf, exists: bool, content_hash: String) -> Self {
        Self { path, exists, content_hash }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NoOpPatchDecision {
    Execute,
    Success { signature: String, occurrence: u8 },
    Block { signature: String, occurrence: u8 },
}

#[derive(Debug, Default)]
pub(super) struct NoOpPatchGuard {
    last_signature: Option<String>,
    occurrence: u8,
}

impl NoOpPatchGuard {
    /// Classify the patch currently being applied against the no-op ladder.
    ///
    /// Returns [`NoOpPatchDecision::Execute`] when the patch changes at least
    /// one file, [`NoOpPatchDecision::Success`] for the first two identical
    /// no-ops, and [`NoOpPatchDecision::Block`] for the third. The ladder is
    /// keyed by the canonical patch signature plus the before/after path
    /// states, so a materially different retry — or the same retry after
    /// productive work — starts over instead of escalating.
    ///
    /// `canonical_operations` must be the normalised form of `patch`: raw
    /// transport text and path aliases would otherwise hash differently and
    /// defeat the repeat detection.
    pub(super) fn observe(
        &mut self,
        patch: &Patch,
        canonical_operations: &[CanonicalPatchOperation],
        before: &[PatchPathState],
        after: &[PatchPathState],
    ) -> NoOpPatchDecision {
        if before != after {
            self.reset();
            return NoOpPatchDecision::Execute;
        }

        let signature = patch_signature(patch, canonical_operations, before, after);
        if self.last_signature.as_deref() == Some(signature.as_str()) {
            self.occurrence = self.occurrence.saturating_add(1);
        } else {
            self.last_signature = Some(signature.clone());
            self.occurrence = 1;
        }

        if self.occurrence >= 3 {
            NoOpPatchDecision::Block { signature, occurrence: self.occurrence }
        } else {
            NoOpPatchDecision::Success { signature, occurrence: self.occurrence }
        }
    }

    /// Forget the previous signature so the next no-op starts the ladder again.
    ///
    /// Called whenever a patch actually changes something: the ladder exists to
    /// catch repeats of one stuck request, not identical requests separated by
    /// productive work.
    fn reset(&mut self) {
        self.last_signature = None;
        self.occurrence = 0;
    }
}

/// Hash the identity of a no-op request.
///
/// Covers the patch contents, the canonicalised paths, and the before/after
/// state of every touched path, so two requests share a signature only when
/// repeating one would be genuinely identical. See [`encode_patch`] and
/// [`encode_states`] for the encoding.
fn patch_signature(
    patch: &Patch,
    canonical_operations: &[CanonicalPatchOperation],
    before: &[PatchPathState],
    after: &[PatchPathState],
) -> String {
    let mut encoded = String::new();
    encode_patch(&mut encoded, patch, canonical_operations);
    encode_states(&mut encoded, 'B', before);
    encode_states(&mut encoded, 'A', after);
    format!("sha256:{}", vtcode_commons::utils::calculate_sha256(encoded.as_bytes()))
}

/// Append the patch's operations to the signature encoding.
///
/// Every field is length-prefixed so concatenation cannot alias: without it
/// `("ab", "c")` and `("a", "bc")` would encode identically.
fn encode_patch(encoded: &mut String, patch: &Patch, canonical_operations: &[CanonicalPatchOperation]) {
    debug_assert_eq!(patch.operations().len(), canonical_operations.len());
    encoded.push_str(&patch.operations().len().to_string());
    encoded.push(';');
    for (operation, canonical) in patch.operations().iter().zip(canonical_operations) {
        match operation {
            PatchOperation::AddFile { content, .. } => {
                encoded.push('A');
                push_field(encoded, &canonical.source.to_string_lossy());
                push_field(encoded, content);
            }
            PatchOperation::DeleteFile { .. } => {
                encoded.push('D');
                push_field(encoded, &canonical.source.to_string_lossy());
            }
            PatchOperation::UpdateFile { chunks, .. } => {
                encoded.push('U');
                push_field(encoded, &canonical.source.to_string_lossy());
                let destination = canonical
                    .destination
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                push_field(encoded, &destination);
                for chunk in chunks {
                    encoded.push('H');
                    push_field(encoded, chunk.change_context().unwrap_or_default());
                    encoded.push(if chunk.is_end_of_file() { '1' } else { '0' });
                    for line in chunk.lines() {
                        encoded.push(match line {
                            PatchLine::Context(_) => ' ',
                            PatchLine::Addition(_) => '+',
                            PatchLine::Removal(_) => '-',
                        });
                        push_field(encoded, line.as_str());
                    }
                }
            }
        }
    }
}

/// Append one side of the before/after state to the signature encoding.
///
/// States are sorted by path so that the order a patch happens to list its
/// operations in cannot change the signature.
fn encode_states(encoded: &mut String, marker: char, states: &[PatchPathState]) {
    encoded.push(marker);
    let mut states = states.to_vec();
    states.sort_by(|left, right| left.path.cmp(&right.path));
    for state in states {
        push_field(encoded, &state.path.to_string_lossy());
        encoded.push(if state.exists { '1' } else { '0' });
        push_field(encoded, &state.content_hash);
    }
}

/// Append one length-prefixed field to the signature encoding.
fn push_field(encoded: &mut String, value: &str) {
    encoded.push_str(&value.len().to_string());
    encoded.push(':');
    encoded.push_str(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Build a single-path state fixture for the guard property tests.
    fn state(path: &str, hash: &str) -> PatchPathState {
        PatchPathState::new(PathBuf::from(path), true, hash.to_string())
    }

    /// Build a one-operation fixture targeting the given path.
    fn operation(path: &str) -> Vec<CanonicalPatchOperation> {
        vec![CanonicalPatchOperation { source: PathBuf::from(path), destination: None }]
    }

    /// Check LF, CRLF, and `./` aliases normalise to the same signature.
    #[test]
    fn parsed_transport_and_path_aliases_share_one_signature() {
        let lf = Patch::parse("*** Begin Patch\n*** Update File: x.txt\n@@\n-x\n+x\n*** End Patch\n")
            .expect("valid LF patch");
        let crlf_alias =
            Patch::parse("*** Begin Patch\r\n*** Update File: ./x.txt\r\n@@\r\n-x\r\n+x\r\n*** End Patch\r\n")
                .expect("valid CRLF patch");
        let canonical = operation("/workspace/x.txt");
        let states = vec![state("/workspace/x.txt", "same")];

        assert_eq!(
            patch_signature(&lf, &canonical, &states, &states),
            patch_signature(&crlf_alias, &canonical, &states, &states)
        );
    }

    proptest! {
        /// Check the signature ignores the order paths are listed in.
        #[test]
        fn patch_guard_prop_signature_is_path_order_invariant(
            left_path in "[a-z]{1,8}\\.txt",
            right_path in "[a-z]{1,8}\\.txt",
            left_hash in "[0-9a-f]{64}",
            right_hash in "[0-9a-f]{64}",
        ) {
            prop_assume!(left_path != right_path);
            let patch = Patch::parse("*** Begin Patch\n*** Add File: x.txt\n+x\n*** End Patch\n")
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let forward = vec![state(&left_path, &left_hash), state(&right_path, &right_hash)];
            let reverse = vec![state(&right_path, &right_hash), state(&left_path, &left_hash)];
            let operations = operation("x.txt");
            prop_assert_eq!(
                patch_signature(&patch, &operations, &forward, &forward),
                patch_signature(&patch, &operations, &reverse, &reverse)
            );
        }

        /// Check a changed patch or file state resets the no-op ladder.
        #[test]
        fn patch_guard_prop_state_or_patch_changes_reset_the_ladder(
            suffix in "[a-z]{1,12}",
        ) {
            prop_assume!(suffix != "x");
            let first_patch = Patch::parse("*** Begin Patch\n*** Add File: x.txt\n+x\n*** End Patch\n")
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let second_patch = Patch::parse(&format!("*** Begin Patch\n*** Add File: x.txt\n+{suffix}\n*** End Patch\n"))
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let first_state = vec![state("x.txt", "first")];
            let second_state = vec![state("x.txt", "second")];
            let operations = operation("x.txt");
            let mut guard = NoOpPatchGuard::default();
            let _ = guard.observe(&first_patch, &operations, &first_state, &first_state);
            let _ = guard.observe(&first_patch, &operations, &first_state, &first_state);

            prop_assert!(matches!(
                guard.observe(&second_patch, &operations, &first_state, &first_state),
                NoOpPatchDecision::Success { occurrence: 1, .. }
            ), "changed patch should reset the ladder");
            prop_assert!(matches!(
                guard.observe(&second_patch, &operations, &second_state, &second_state),
                NoOpPatchDecision::Success { occurrence: 1, .. }
            ), "changed file state should reset the ladder");
        }
    }
}
