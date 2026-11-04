//! Boundary between model-facing tracker responses and filesystem paths.
//!
//! Tool responses intentionally use workspace-relative paths. The runloop
//! must resolve those values explicitly before it performs filesystem work so
//! a response cannot accidentally be interpreted relative to the process cwd.

use anyhow::{Context, Result, bail};
use diapason_commons::paths::ensure_path_within_workspace;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::tracker_file_for_plan_file;

pub(crate) fn resolve_tracker_file_response(
    result: &Value,
    workspace_root: Option<&Path>,
    plan_file: &Path,
) -> Result<PathBuf> {
    let fallback = tracker_file_for_plan_file(plan_file).unwrap_or_else(|| {
        plan_file.with_file_name(format!(
            "{}.tasks.md",
            plan_file.file_stem().and_then(|stem| stem.to_str()).unwrap_or("plan")
        ))
    });

    let Some(raw_path) = result
        .get("tracker_file")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return validate_candidate(fallback, workspace_root);
    };

    let path = PathBuf::from(raw_path);
    let Some(workspace_root) = workspace_root else {
        if path.is_absolute() {
            return Ok(path);
        }
        bail!("relative tracker_file response requires a workspace root");
    };

    let candidate = if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    };
    validate_candidate(candidate, Some(workspace_root))
}

fn validate_candidate(candidate: PathBuf, workspace_root: Option<&Path>) -> Result<PathBuf> {
    let Some(workspace_root) = workspace_root else {
        return Ok(candidate);
    };

    ensure_path_within_workspace(&candidate, workspace_root)
        .with_context(|| format!("tracker_file response escapes workspace: {}", candidate.display()))
}
