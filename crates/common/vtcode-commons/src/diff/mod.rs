#![expect(
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    unused_results,
    reason = "Diff ranges and offsets use one character/byte mapping; discarded map updates are intentional."
)]

//! Structured and character-level diff utilities.

mod chunks;
mod structured;

pub use chunks::{Chunk, compute_diff_chunks};
pub use structured::{DiffBundle, DiffHunk, DiffLine, DiffLineKind, DiffOptions, compute_diff};
