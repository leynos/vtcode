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

/// Return original operation indices in canonical encoding order.
///
/// The applicator replays operations in order and carries pending content per
/// path, so two operations that touch a common path must keep their supplied
/// relative order: swapping them could describe different final bytes.
/// Independent operations carry no such dependency, so they are emitted
/// deterministically — smallest canonical source path bytes first, with the
/// original index as a tie-break. That makes the signature invariant to the
/// order a caller happened to list unrelated edits, so alternating two
/// equivalent orderings cannot reset the no-op ladder.
///
/// For each operation the touched paths are its canonical source plus its
/// destination when present. Only interacting pairs (earlier-supplied to
/// later-supplied) add an edge, which keeps the graph acyclic and preserves
/// the supplied order of dependent operations.
fn canonical_operation_order(canonical_operations: &[CanonicalPatchOperation]) -> Vec<usize> {
    let count = canonical_operations.len();
    if count <= 1 {
        return (0..count).collect();
    }

    let touched_paths: Vec<Vec<&std::path::Path>> = canonical_operations
        .iter()
        .map(|operation| {
            let mut paths = vec![operation.source.as_path()];
            if let Some(destination) = operation.destination.as_deref() {
                paths.push(destination);
            }
            paths
        })
        .collect();

    let mut indegree = vec![0usize; count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (earlier, earlier_paths) in touched_paths.iter().enumerate() {
        for (later, later_paths) in touched_paths.iter().enumerate().skip(earlier + 1) {
            let interacts = earlier_paths.iter().any(|left| later_paths.iter().any(|right| left == right));
            if !interacts {
                continue;
            }
            if let Some(degree) = indegree.get_mut(later) {
                *degree += 1;
            }
            if let Some(edges) = dependents.get_mut(earlier) {
                edges.push(later);
            }
        }
    }

    let mut emitted = vec![false; count];
    let mut order = Vec::with_capacity(count);
    while order.len() < count {
        let next = indegree
            .iter()
            .enumerate()
            .filter(|(index, degree)| **degree == 0 && !emitted.get(*index).copied().unwrap_or(true))
            .map(|(index, _)| index)
            .min_by_key(|index| operation_order_key(canonical_operations, *index));
        let Some(chosen) = next else {
            break;
        };
        if let Some(flag) = emitted.get_mut(chosen) {
            *flag = true;
        }
        order.push(chosen);
        if let Some(edges) = dependents.get(chosen) {
            for dependent in edges {
                if let Some(degree) = indegree.get_mut(*dependent) {
                    *degree = degree.saturating_sub(1);
                }
            }
        }
    }
    order
}

/// Ordering key for canonical emission: source path bytes, then original index.
fn operation_order_key(canonical_operations: &[CanonicalPatchOperation], index: usize) -> (&[u8], usize) {
    let source = canonical_operations
        .get(index)
        .map(|operation| operation.source.as_os_str().as_encoded_bytes())
        .unwrap_or_default();
    (source, index)
}

/// Append the patch's operations to the signature encoding.
///
/// Every field is length-prefixed so concatenation cannot alias: without it
/// `("ab", "c")` and `("a", "bc")` would encode identically. Operations are
/// emitted in the canonical order from [`canonical_operation_order`], paired
/// back with their canonical path by original index.
fn encode_patch(encoded: &mut String, patch: &Patch, canonical_operations: &[CanonicalPatchOperation]) {
    debug_assert_eq!(patch.operations().len(), canonical_operations.len());
    encoded.push_str(&patch.operations().len().to_string());
    encoded.push(';');
    for original_index in canonical_operation_order(canonical_operations) {
        let Some(operation) = patch.operations().get(original_index) else {
            continue;
        };
        let Some(canonical) = canonical_operations.get(original_index) else {
            continue;
        };
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
mod tests;
