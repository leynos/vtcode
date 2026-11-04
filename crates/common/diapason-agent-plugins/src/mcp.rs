use std::collections::HashMap;

use crate::errors::PluginError;

const SUPPORTED_SCHEMAS: &[&str] = &["https://agent-plugins.org/schemas/1.0.0/mcp.schema.json"];

#[derive(Debug, Clone)]
pub struct McpConfig {
    pub schema: String,
    pub servers: HashMap<String, ServerConfig>,
    pub unknown_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ServerConfig {
    Stdio(StdioServerConfig),
    StreamableHttp(HttpServerConfig),
    Sse(SseServerConfig),
}

#[derive(Debug, Clone)]
pub struct StdioServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    pub url: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct SseServerConfig {
    pub url: String,
    pub headers: HashMap<String, String>,
}

impl McpConfig {
    pub fn parse(content: &str) -> Result<Self, PluginError> {
        let value: serde_json::Value = serde_json::from_str(content).map_err(PluginError::Json)?;
        let map = value
            .as_object()
            .ok_or_else(|| PluginError::InvalidMcp("mcp.json must be a JSON object".into()))?;

        let mut unknown = Vec::new();
        let mut config = McpConfig {
            schema: String::new(),
            servers: HashMap::new(),
            unknown_fields: Vec::new(),
        };

        for (key, val) in map {
            match key.as_str() {
                "$schema" => {
                    let s = val
                        .as_str()
                        .ok_or_else(|| PluginError::InvalidMcp("$schema must be a string".into()))?;
                    config.schema = s.to_string();
                }
                "mcpServers" => {
                    let servers_map = val
                        .as_object()
                        .ok_or_else(|| PluginError::InvalidMcp("mcpServers must be an object".into()))?;
                    for (name, server_val) in servers_map {
                        match parse_server_config(name, server_val) {
                            Ok(server) => {
                                drop(config.servers.insert(name.clone(), server));
                            }
                            // Per the Agent Plugins spec, an invalid individual
                            // server entry is skipped while valid peers continue
                            // loading. Both missing-field and wrong-type errors
                            // are entry-local failures — one bad server must not
                            // disable every MCP server in the plugin.
                            Err(ServerConfigError::MissingField(msg)) => {
                                tracing::warn!(
                                    server = name,
                                    error = msg,
                                    "skipping MCP server entry with missing required fields"
                                );
                            }
                            Err(ServerConfigError::WrongType(msg)) => {
                                tracing::warn!(
                                    server = name,
                                    error = msg,
                                    "skipping MCP server entry with wrong-typed fields"
                                );
                            }
                        }
                    }
                }
                _ => unknown.push(key.clone()),
            }
        }

        if config.schema.is_empty() {
            return Err(PluginError::InvalidMcp("missing required field: $schema".into()));
        }
        if !SUPPORTED_SCHEMAS.contains(&config.schema.as_str()) {
            return Err(PluginError::InvalidMcp(format!(
                "unsupported $schema '{}' (expected {})",
                config.schema,
                SUPPORTED_SCHEMAS.join(" or ")
            )));
        }

        config.unknown_fields = unknown;
        Ok(config)
    }
}

/// Distinguishes missing required fields from wrong-typed fields during
/// MCP server config parsing. Both are entry-local failures: the individual
/// server is skipped while valid peers continue loading (Agent Plugins spec).
/// The distinction improves diagnostic messages so users can tell whether a
/// field was absent or had the wrong JSON type.
enum ServerConfigError {
    MissingField(String),
    WrongType(String),
}

/// Extract a required string field, distinguishing absent from wrong-typed.
fn require_string_field<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    name: &'a str,
    field: &str,
) -> Result<&'a str, ServerConfigError> {
    match obj.get(field) {
        Some(serde_json::Value::String(s)) => Ok(s),
        Some(_) => Err(ServerConfigError::WrongType(format!("server '{name}' field '{field}' must be a string"))),
        None => Err(ServerConfigError::MissingField(format!("server '{name}' missing required field: {field}"))),
    }
}

fn parse_server_config(name: &str, value: &serde_json::Value) -> Result<ServerConfig, ServerConfigError> {
    let obj = value
        .as_object()
        .ok_or_else(|| ServerConfigError::WrongType(format!("server '{}' must be an object", name)))?;

    let type_val = require_string_field(obj, name, "type")?;

    match type_val {
        "stdio" => {
            let command = require_string_field(obj, name, "command")?.to_string();

            let args = obj
                .get("args")
                .map(|v| expect_string_array(v, &format!("server '{name}' args")))
                .transpose()?
                .unwrap_or_default();

            let env = obj
                .get("env")
                .map(|v| expect_string_map(v, &format!("server '{name}' env")))
                .transpose()?
                .unwrap_or_default();

            let cwd = obj
                .get("cwd")
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| ServerConfigError::WrongType(format!("server '{name}' cwd must be a string")))
                })
                .transpose()?;

            Ok(ServerConfig::Stdio(StdioServerConfig { command, args, env, cwd }))
        }
        "streamable-http" => {
            let url = require_string_field(obj, name, "url")?.to_string();

            let headers = obj
                .get("headers")
                .map(|v| expect_string_map(v, &format!("server '{name}' headers")))
                .transpose()?
                .unwrap_or_default();

            Ok(ServerConfig::StreamableHttp(HttpServerConfig { url, headers }))
        }
        "sse" => {
            let url = require_string_field(obj, name, "url")?.to_string();

            let headers = obj
                .get("headers")
                .map(|v| expect_string_map(v, &format!("server '{name}' headers")))
                .transpose()?
                .unwrap_or_default();

            Ok(ServerConfig::Sse(SseServerConfig { url, headers }))
        }
        _ => Err(ServerConfigError::WrongType(format!("server '{}' has unsupported type: {}", name, type_val))),
    }
}

fn expect_string_array(value: &serde_json::Value, context: &str) -> Result<Vec<String>, ServerConfigError> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| ServerConfigError::WrongType(format!("{context} must contain only strings")))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_else(|| Err(ServerConfigError::WrongType(format!("{context} must be an array"))))
}

fn expect_string_map(value: &serde_json::Value, context: &str) -> Result<HashMap<String, String>, ServerConfigError> {
    value
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k.clone(), s.to_string()))
                        .ok_or_else(|| ServerConfigError::WrongType(format!("{context}.{k} must be a string")))
                })
                .collect::<Result<HashMap<_, _>, _>>()
        })
        .unwrap_or_else(|| Err(ServerConfigError::WrongType(format!("{context} must be an object"))))
}
