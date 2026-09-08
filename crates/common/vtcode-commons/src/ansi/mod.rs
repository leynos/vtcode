//! Shared ANSI escape parser and stripping utilities for VT Code.
//!
//! See `docs/reference/ansi-in-vtcode.md` for the workspace usage map.

mod parser;

pub use parser::{contains_unicode, parse_ansi_sequence, strip_ansi, strip_ansi_ascii_only, strip_ansi_codes};
