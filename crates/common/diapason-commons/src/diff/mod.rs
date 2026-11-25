//! Structured and character-level diff utilities.

mod chunks;
mod structured;

pub use chunks::{Chunk, compute_diff_chunks};
pub use structured::{DiffBundle, DiffHunk, DiffLine, DiffLineKind, DiffOptions, compute_diff};
