//! Tests for TOML value insertion and merge provenance.

use super::*;

#[test]
fn test_insert_dotted_key() {
    let mut table = toml::Table::new();
    ConfigBuilder::insert_dotted_key(&mut table, "a.b.c", toml::Value::String("value".to_string()))
        .expect("insert_dotted_key should succeed for a.b.c");

    let a = table
        .get("a")
        .expect("inserted dotted key should have an a component")
        .as_table()
        .expect("a should be a table");
    let b = a
        .get("b")
        .expect("inserted dotted key should have a b component")
        .as_table()
        .expect("b should be a table");
    let c = b
        .get("c")
        .expect("inserted dotted key should have a c component")
        .as_str()
        .expect("c should be a string");
    assert_eq!(c, "value");
}

#[test]
fn test_merge_toml_values() {
    let mut base = toml::from_str::<toml::Value>(
        r#"
            [agent]
            provider = "openai"
            [tools]
            default_policy = "prompt"
        "#,
    )
    .expect("base TOML fixture should parse");

    let overlay = toml::from_str::<toml::Value>(
        r#"
            [agent]
            provider = "anthropic"
            default_model = "claude-3"
        "#,
    )
    .expect("overlay TOML fixture should parse");

    merge_toml_values(&mut base, &overlay);

    let agent = base
        .get("agent")
        .expect("merged value should have an agent section")
        .as_table()
        .expect("agent should be a table");
    assert_eq!(
        agent
            .get("provider")
            .expect("merged agent should have a provider")
            .as_str()
            .expect("provider should be a string"),
        "anthropic"
    );
    assert_eq!(
        agent
            .get("default_model")
            .expect("merged agent should have a default model")
            .as_str()
            .expect("default model should be a string"),
        "claude-3"
    );

    let tools = base
        .get("tools")
        .expect("merged value should retain a tools section")
        .as_table()
        .expect("tools should be a table");
    assert_eq!(
        tools
            .get("default_policy")
            .expect("merged tools should retain default_policy")
            .as_str()
            .expect("default_policy should be a string"),
        "prompt"
    );
}

#[test]
fn test_merge_toml_values_with_origins_tracks_winning_layer() {
    use crate::loader::layers::ConfigLayerMetadata;

    let mut base = toml::from_str::<toml::Value>(
        r#"
            [agent]
            provider = "openai"
        "#,
    )
    .expect("base TOML fixture should parse");

    let overlay = toml::from_str::<toml::Value>(
        r#"
            [agent]
            provider = "anthropic"
        "#,
    )
    .expect("overlay TOML fixture should parse");

    let layer = ConfigLayerMetadata {
        name: "workspace:/tmp/vtcode.toml".to_string(),
        version: "abc123".to_string(),
    };
    let mut origins = hashbrown::HashMap::new();
    merge_toml_values_with_origins(&mut base, &overlay, &mut origins, &layer);

    assert_eq!(
        base.get("agent")
            .and_then(|agent| agent.get("provider"))
            .and_then(toml::Value::as_str),
        Some("anthropic")
    );
    assert_eq!(origins.get("agent.provider"), Some(&layer));
}
