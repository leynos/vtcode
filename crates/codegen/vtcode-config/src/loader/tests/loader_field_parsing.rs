//! Tests for individual loader fields parsed from TOML.

use super::*;

#[test]
fn loader_loads_prompt_cache_retention_from_toml() {
    use std::fs::File;
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("vtcode.toml");
    let mut file = File::create(&path).unwrap();
    let contents = r#"
[prompt_cache]
enabled = true
cache_friendly_prompt_shaping = true
[prompt_cache.providers.openai]
prompt_cache_retention = "24h"
prompt_cache_key_mode = "off"
"#;
    file.write_all(contents.as_bytes()).unwrap();

    let manager = ConfigManager::load_from_file(&path).unwrap();
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

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("vtcode.toml");
    let mut file = File::create(&path).unwrap();
    let contents = r#"
[tools.editor]
enabled = true
preferred_editor = "code --wait"
suspend_tui = false
"#;
    file.write_all(contents.as_bytes()).unwrap();

    let manager = ConfigManager::load_from_file(&path).unwrap();
    let config = manager.config();
    assert!(config.tools.editor.enabled);
    assert_eq!(config.tools.editor.preferred_editor, "code --wait");
    assert!(!config.tools.editor.suspend_tui);
}
