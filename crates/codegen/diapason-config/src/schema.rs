use anyhow::{Context, Result};
use schemars::{Schema, schema_for};

use crate::loader::DiapasonConfig;

/// Generate the JSON Schema describing the `config.toml` configuration surface.
pub fn diapason_config_schema() -> Schema {
    schema_for!(DiapasonConfig)
}

/// Render the configuration schema as a `serde_json::Value` for downstream tooling.
pub fn diapason_config_schema_json() -> Result<serde_json::Value> {
    let schema = diapason_config_schema();
    serde_json::to_value(schema).context("failed to serialize diapason-config schema to JSON value")
}

/// Render the configuration schema as a pretty-printed JSON string.
pub fn diapason_config_schema_pretty() -> Result<String> {
    let value = diapason_config_schema_json()?;
    serde_json::to_string_pretty(&value).context("failed to serialize diapason-config schema to pretty JSON string")
}
