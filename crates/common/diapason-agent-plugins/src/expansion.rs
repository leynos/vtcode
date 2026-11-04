use std::path::{Path, PathBuf};

pub fn expand_placeholders(value: &str, plugin_root: &Path, plugin_data: &Path) -> String {
    value
        .replace("${PLUGIN_ROOT}", &plugin_root.display().to_string())
        .replace("${PLUGIN_DATA}", &plugin_data.display().to_string())
}

/// Validate that a plugin-relative path stays inside the plugin root.
///
/// Uses `canonicalize` (which resolves symlinks) so a symlink inside the
/// plugin pointing outside (e.g. `bin/link -> /etc`) cannot smuggle a path out
/// of the sandbox. The candidate must start with `./` per the Agent Plugins
/// spec.
///
/// The target need not exist yet (a plugin may spawn a binary built at
/// runtime), so when it is absent we canonicalize the deepest *existing*
/// ancestor and verify the remaining suffix stays lexically inside the
/// canonicalized root.
pub fn validate_plugin_relative(value: &str, root: &Path) -> Result<PathBuf, crate::PluginError> {
    if !value.starts_with("./") {
        return Err(crate::PluginError::PathEscape(format!("path must start with ./: {}", value)));
    }

    let canonical_root = diapason_commons::canonicalize(root)
        .map_err(|e| crate::PluginError::PathEscape(format!("plugin root could not be resolved: {e}")))?;

    let candidate = root.join(value.get(2..).unwrap_or_default());

    // Reject obvious lexical escapes before touching the filesystem.
    let lexical_ok = candidate
        .components()
        .all(|component| !matches!(component, std::path::Component::ParentDir));
    if !lexical_ok {
        return Err(crate::PluginError::PathEscape(format!("path escapes plugin root: {}", value)));
    }

    // Walk up to the deepest existing ancestor and canonicalize it; the
    // remaining suffix is appended lexically.
    let mut probe = candidate.as_path();
    let mut suffix: Vec<PathBuf> = Vec::new();
    loop {
        match diapason_commons::canonicalize(probe) {
            Ok(existing) => {
                let mut resolved = existing;
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                if !resolved.starts_with(&canonical_root) {
                    return Err(crate::PluginError::PathEscape(format!("path escapes plugin root: {}", value)));
                }
                return Ok(resolved);
            }
            Err(_) => {
                let Some(name) = probe.file_name() else {
                    return Err(crate::PluginError::PathEscape(format!("path could not be resolved: {}", value)));
                };
                suffix.push(PathBuf::from(name));
                let Some(parent) = probe.parent() else {
                    return Err(crate::PluginError::PathEscape(format!("path could not be resolved: {}", value)));
                };
                probe = parent;
            }
        }
    }
}
