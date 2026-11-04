use crate::config::DiapasonConfig;
use diapason_commons::DiapasonPaths;
use serde_json::json;

/// Apply backward-compatible defaults for new config sections.
pub fn apply_migration_defaults(config: &mut DiapasonConfig) {
    if config.tools.plugins.manifests.is_empty() {
        let plugin_root = DiapasonPaths::resolve()
            .map(|paths| paths.plugins_dir().display().to_string())
            .unwrap_or_else(|_| String::from("data/diapason/plugins"));
        config.tools.plugins.manifests = vec![plugin_root];
    }
}

/// Emit a structured migration summary for callers.
pub fn migration_summary(config: &DiapasonConfig) -> serde_json::Value {
    json!({
        "plugins": {
            "enabled": config.tools.plugins.enabled,
            "manifests": config.tools.plugins.manifests,
        },
        "security": {
            "zero_trust": config.security.zero_trust_mode,
            "integrity_checks": config.security.integrity_checks,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_missing_defaults() {
        let mut config = DiapasonConfig::default();
        apply_migration_defaults(&mut config);
        assert!(!config.tools.plugins.manifests.is_empty());
    }
}
