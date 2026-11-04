use std::collections::HashMap;

use crate::errors::PluginError;

const SUPPORTED_SCHEMAS: &[&str] = &["https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"];

#[derive(Debug, Clone)]
pub struct PluginAuthor {
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PluginManifest {
    pub schema: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<PluginAuthor>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub extensions: Option<HashMap<String, serde_json::Value>>,
    pub unknown_fields: Vec<String>,
}

impl PluginManifest {
    pub fn parse(content: &str) -> Result<(Self, Vec<String>), PluginError> {
        let value: serde_json::Value = serde_json::from_str(content).map_err(PluginError::Json)?;
        let map = value
            .as_object()
            .ok_or_else(|| PluginError::InvalidManifest("manifest must be a JSON object".into()))?;

        let mut unknown = Vec::new();
        let mut manifest = PluginManifest {
            schema: String::new(),
            name: String::new(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: None,
            extensions: None,
            unknown_fields: Vec::new(),
        };

        for (key, val) in map {
            match key.as_str() {
                "$schema" => {
                    let s = val
                        .as_str()
                        .ok_or_else(|| PluginError::InvalidManifest("$schema must be a string".into()))?;
                    manifest.schema = s.to_string();
                }
                "name" => {
                    manifest.name = val
                        .as_str()
                        .ok_or_else(|| PluginError::InvalidManifest("name must be a string".into()))?
                        .to_string();
                }
                "version" => {
                    manifest.version = Some(expect_string(val, "version")?);
                }
                "description" => {
                    manifest.description = Some(expect_string(val, "description")?);
                }
                "author" => {
                    manifest.author = Some(parse_author(val)?);
                }
                "homepage" => {
                    manifest.homepage = Some(expect_string(val, "homepage")?);
                }
                "repository" => {
                    manifest.repository = Some(expect_string(val, "repository")?);
                }
                "license" => {
                    manifest.license = Some(expect_string(val, "license")?);
                }
                "keywords" => {
                    manifest.keywords = Some(expect_string_array(val, "keywords")?);
                }
                "extensions" => {
                    // Per the Agent Plugins spec, a non-object `extensions`
                    // value (including null) is reported and ignored — the
                    // plugin continues loading. Only object values are stored.
                    match val.as_object() {
                        Some(obj) => {
                            manifest.extensions = Some(obj.into_iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                        }
                        None => {
                            tracing::warn!(field = "extensions", "extensions is not an object; ignoring");
                        }
                    }
                }
                _ => unknown.push(key.clone()),
            }
        }

        if manifest.schema.is_empty() {
            return Err(PluginError::InvalidManifest("missing required field: $schema".into()));
        }
        if !SUPPORTED_SCHEMAS.contains(&manifest.schema.as_str()) {
            return Err(PluginError::InvalidManifest(format!(
                "unsupported $schema '{}' (expected {})",
                manifest.schema,
                SUPPORTED_SCHEMAS.join(" or ")
            )));
        }
        if manifest.name.is_empty() {
            return Err(PluginError::InvalidManifest("missing required field: name".into()));
        }

        Self::validate_name(&manifest.name).map_err(PluginError::InvalidName)?;

        Ok((manifest, unknown))
    }

    pub fn validate_name(name: &str) -> Result<(), String> {
        if name.is_empty() || name.len() > 64 {
            return Err("name must be 1-64 characters".into());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        {
            return Err("name must contain only a-z, 0-9, -, .".into());
        }
        if !name.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !name.ends_with(|c: char| c.is_ascii_alphanumeric())
        {
            return Err("name must start and end with an alphanumeric character".into());
        }
        if name.contains("--") || name.contains("..") {
            return Err("name must not contain consecutive hyphens or periods".into());
        }
        Ok(())
    }
}

fn expect_string(value: &serde_json::Value, field: &str) -> Result<String, PluginError> {
    value
        .as_str()
        .map(String::from)
        .ok_or_else(|| PluginError::InvalidManifest(format!("{field} must be a string")))
}

fn expect_string_array(value: &serde_json::Value, field: &str) -> Result<Vec<String>, PluginError> {
    let arr = value
        .as_array()
        .ok_or_else(|| PluginError::InvalidManifest(format!("{field} must be an array")))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(String::from)
                .ok_or_else(|| PluginError::InvalidManifest(format!("{field} must contain only strings")))
        })
        .collect()
}

fn parse_author(value: &serde_json::Value) -> Result<PluginAuthor, PluginError> {
    let obj = value
        .as_object()
        .ok_or_else(|| PluginError::InvalidManifest("author must be an object".into()))?;

    let mut author = PluginAuthor { name: None, email: None, url: None };

    for (key, val) in obj {
        match key.as_str() {
            "name" | "email" | "url" => {
                let v = val
                    .as_str()
                    .ok_or_else(|| PluginError::InvalidManifest(format!("author.{} must be a string", key)))?;
                if key == "name" {
                    author.name = Some(v.to_string());
                } else if key == "email" {
                    author.email = Some(v.to_string());
                } else {
                    author.url = Some(v.to_string());
                }
            }
            _ => {
                return Err(PluginError::InvalidManifest(format!("author contains unsupported field: {}", key)));
            }
        }
    }

    Ok(author)
}
