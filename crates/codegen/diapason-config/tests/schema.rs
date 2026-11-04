//! Integration tests for the diapason-config JSON Schema export.

#![cfg(feature = "schema")]
#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

use diapason_config::{diapason_config_schema, diapason_config_schema_json, diapason_config_schema_pretty};

#[test]
fn schema_helpers_produce_consistent_output() {
    let root = diapason_config_schema();
    let as_json = diapason_config_schema_json().expect("schema to serialize to JSON value");
    assert!(as_json.is_object(), "expected JSON schema representation to be an object");

    let as_string = diapason_config_schema_pretty().expect("schema to serialize to JSON string");
    assert!(
        as_string.contains("\"title\": \"DiapasonConfig\""),
        "pretty schema output should mention the root configuration type"
    );

    // Ensure both helper paths describe the same schema title.
    let title_from_value = as_json
        .get("title")
        .and_then(|value| value.as_str())
        .expect("schema JSON should include a title");
    let title_from_root = root.get("title").and_then(|value| value.as_str()).unwrap_or("");
    assert_eq!(title_from_value, title_from_root);

    assert!(as_string.contains("\"vim_mode\""), "pretty schema output should include ui.vim_mode");
    assert!(
        as_string.contains("\"notification\""),
        "pretty schema output should include hooks.lifecycle.notification"
    );
    assert!(
        as_string.contains("\"default_policy\""),
        "pretty schema output should include sandbox.default_policy"
    );
    let removed_field = format!("default_{}", "mode");
    assert!(
        !as_string.contains(&format!("\"{removed_field}\"")),
        "pretty schema output should not include sandbox.{removed_field}"
    );
}
