//! Tests for syntax highlighting defaults and validation.

use super::*;

#[test]
#[serial]
fn syntax_highlighting_defaults_are_valid() {
    let config = SyntaxHighlightingConfig::default();
    config.validate().expect("default syntax highlighting config should be valid");
    // Default is empty — all syntect grammars enabled
    assert!(
        config.enabled_languages.is_empty(),
        "default enabled_languages should be empty to allow all syntect grammars"
    );
}

#[test]
fn vtcode_config_validation_fails_for_invalid_highlight_timeout() {
    let mut config = VTCodeConfig::default();
    config.syntax_highlighting.highlight_timeout_ms = 0;
    let error = config
        .validate()
        .expect_err("validation should fail for zero highlight timeout");
    assert!(format!("{error:#}").contains("highlight"), "expected error to mention highlight, got: {error:#}");
}

#[test]
fn load_from_file_rejects_invalid_syntax_highlighting() {
    let mut temp_file = NamedTempFile::new().expect("failed to create temp file");
    writeln!(temp_file, "[syntax_highlighting]\nhighlight_timeout_ms = 0\n").expect("failed to write temp config");

    let result = ConfigManager::load_from_file(temp_file.path());
    assert!(result.is_err(), "expected validation error");
    let error = format!("{:?}", result.err().expect("invalid highlighting configuration should be rejected"));
    assert!(error.contains("validate"), "expected validation context in error, got: {error}");
}
