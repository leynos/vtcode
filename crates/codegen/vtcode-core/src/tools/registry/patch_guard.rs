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

    fn reset(&mut self) {
        self.last_signature = None;
        self.occurrence = 0;
    }
}

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

fn push_field(encoded: &mut String, value: &str) {
    encoded.push_str(&value.len().to_string());
    encoded.push(':');
    encoded.push_str(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn state(path: &str, hash: &str) -> PatchPathState {
        PatchPathState::new(PathBuf::from(path), true, hash.to_string())
    }

    fn operation(path: &str) -> Vec<CanonicalPatchOperation> {
        vec![CanonicalPatchOperation { source: PathBuf::from(path), destination: None }]
    }

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
