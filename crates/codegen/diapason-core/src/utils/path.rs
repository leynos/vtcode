//! Path utility functions
//!
//! Re-exports from diapason-commons for backward compatibility.

pub use diapason_commons::paths::{
    canonicalize_allow_missing, canonicalize_workspace, ensure_path_within_workspace, normalize_path,
};
pub use diapason_commons::paths::{normalize_ascii_identifier, resolve_workspace_path};
