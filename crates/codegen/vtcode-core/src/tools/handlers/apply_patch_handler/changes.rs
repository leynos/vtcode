//! File-change tracking for `apply_patch` operations.
//!
//! The handler emits begin/finish events carrying the paths a patch will
//! touch. Keeping the conversion here holds the handler within the
//! repository's 400-line limit.

use std::path::{Path, PathBuf};

use hashbrown::HashMap;

use super::super::tool_handler::FileChange;
use crate::tools::editing::{Patch, PatchOperation};

/// Convert patch operations to file changes for tracking.
pub(super) fn convert_patch_to_changes(patch: &Patch, cwd: &Path) -> HashMap<PathBuf, FileChange> {
    let mut changes = HashMap::new();

    for op in patch.operations() {
        match op {
            PatchOperation::AddFile { path, content } => {
                let full_path = cwd.join(path);
                changes.insert(full_path, FileChange::Add { content: content.clone() });
            }
            PatchOperation::DeleteFile { path } => {
                let full_path = cwd.join(path);
                changes.insert(full_path, FileChange::Delete);
            }
            PatchOperation::UpdateFile { path, new_path, chunks: _ } => {
                let full_path = cwd.join(path);
                if let Some(new_path) = new_path {
                    changes.insert(full_path, FileChange::Rename { new_path: cwd.join(new_path), content: None });
                } else {
                    // For updates, we track as update with empty placeholders
                    // The actual content will be computed during application
                    changes.insert(
                        full_path,
                        FileChange::Update {
                            old_content: String::new(),
                            new_content: String::new(),
                        },
                    );
                }
            }
        }
    }

    changes
}
