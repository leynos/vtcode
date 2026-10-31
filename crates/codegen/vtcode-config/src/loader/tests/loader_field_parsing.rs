//! Tests for individual loader fields parsed from TOML.

use super::*;

#[test]
fn loader_loads_prompt_cache_retention_from_toml() {
    use std::fs::File;
    use std::io::Write;

    let temp = tempfile::tempdir().expect("failed to create temporary config directory");
    let path = temp.path().join("vtcode.toml");
    let mut file = File::create(&path).expect("failed to create temporary config file");
    let contents = r#"
[prompt_cache]
enabled = true
cache_friendly_prompt_shaping = true
[prompt_cache.providers.openai]
prompt_cache_retention = "24h"
prompt_cache_key_mode = "off"
"#;
    file.write_all(contents.as_bytes())
        .expect("failed to write prompt-cache config");

    let manager = ConfigManager::load_from_file(&path).expect("failed to load prompt-cache config");
    let config = manager.config();
    assert_eq!(config.prompt_cache.providers.openai.prompt_cache_retention, Some(PromptCacheRetention::H24));
    assert_eq!(
        config.prompt_cache.providers.openai.prompt_cache_key_mode,
        crate::core::OpenAIPromptCacheKeyMode::Off
    );
    assert!(config.prompt_cache.cache_friendly_prompt_shaping);
}

#[test]
fn loader_loads_tools_editor_config_from_toml() {
    use std::fs::File;
    use std::io::Write;

    let temp = tempfile::tempdir().expect("failed to create temporary config directory");
    let path = temp.path().join("vtcode.toml");
    let mut file = File::create(&path).expect("failed to create temporary config file");
    let contents = r#"
[tools.editor]
enabled = true
preferred_editor = "code --wait"
suspend_tui = false
"#;
    file.write_all(contents.as_bytes()).expect("failed to write editor config");

    let manager = ConfigManager::load_from_file(&path).expect("failed to load editor config");
    let config = manager.config();
    assert!(config.tools.editor.enabled);
    assert_eq!(config.tools.editor.preferred_editor, "code --wait");
    assert!(!config.tools.editor.suspend_tui);
}
