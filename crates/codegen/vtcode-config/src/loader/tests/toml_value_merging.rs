//! Tests for TOML value insertion and merge provenance.

use super::*;

#[test]
fn test_insert_dotted_key() {
    let mut table = toml::Table::new();
    ConfigBuilder::insert_dotted_key(&mut table, "a.b.c", toml::Value::String("value".to_string()))
        .expect("insert_dotted_key should succeed for a.b.c");

    let a = table.get("a").unwrap().as_table().unwrap();
    let b = a.get("b").unwrap().as_table().unwrap();
    let c = b.get("c").unwrap().as_str().unwrap();
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
    .unwrap();

    let overlay = toml::from_str::<toml::Value>(
        r#"
            [agent]
            provider = "anthropic"
            default_model = "claude-3"
        "#,
    )
    .unwrap();

    merge_toml_values(&mut base, &overlay);

    let agent = base.get("agent").unwrap().as_table().unwrap();
    assert_eq!(agent.get("provider").unwrap().as_str().unwrap(), "anthropic");
    assert_eq!(agent.get("default_model").unwrap().as_str().unwrap(), "claude-3");

    let tools = base.get("tools").unwrap().as_table().unwrap();
    assert_eq!(tools.get("default_policy").unwrap().as_str().unwrap(), "prompt");
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
    .unwrap();

    let overlay = toml::from_str::<toml::Value>(
        r#"
            [agent]
            provider = "anthropic"
        "#,
    )
    .unwrap();

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
