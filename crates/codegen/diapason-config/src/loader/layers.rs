use crate::loader::merge::{merge_toml_values, merge_toml_values_with_origins};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use toml::Value as TomlValue;

use super::fingerprint::fingerprint_toml_value;

/// Source of a configuration layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConfigLayerSource {
    /// System-wide configuration (e.g., /etc/diapason/config.toml)
    System { file: PathBuf },
    /// User-specific configuration (the canonical platform config directory;
    /// files live only in the canonical Diapason XDG configuration tree).
    User { file: PathBuf },
    /// Project-specific configuration (e.g., .diapason/projects/foo/config/config.toml)
    Project { file: PathBuf },
    /// Workspace-specific configuration (e.g., config.toml in workspace root)
    Workspace { file: PathBuf },
    /// Runtime overrides (e.g., CLI flags)
    Runtime,
}

impl ConfigLayerSource {
    /// Lower numbers are lower precedence.
    const fn precedence(&self) -> i16 {
        match self {
            Self::System { .. } => 10,
            Self::User { .. } => 20,
            Self::Project { .. } => 25,
            Self::Workspace { .. } => 30,
            Self::Runtime => 40,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::System { file } => format!("system:{}", file.display()),
            Self::User { file } => format!("user:{}", file.display()),
            Self::Project { file } => format!("project:{}", file.display()),
            Self::Workspace { file } => format!("workspace:{}", file.display()),
            Self::Runtime => "runtime".to_string(),
        }
    }

    /// Return a copy of this source with an updated file path.
    pub(crate) fn with_file(&self, new_file: PathBuf) -> Self {
        match self {
            Self::System { .. } => Self::System { file: new_file },
            Self::User { .. } => Self::User { file: new_file },
            Self::Project { .. } => Self::Project { file: new_file },
            Self::Workspace { .. } => Self::Workspace { file: new_file },
            Self::Runtime => Self::Runtime,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigLayerMetadata {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerDisabledReason {
    ParseError,
    LoadError,
    UntrustedWorkspace,
    PolicyDisabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigLayerLoadError {
    pub message: String,
}

/// Configuration keys that accept a British spelling alias, as
/// `(parent table, alias, canonical wire name)`.
const CONFIG_KEY_ALIASES: &[(&str, &str, &str)] = &[
    ("ui", "safe_colours_only", "safe_colors_only"),
    ("ui", "colour_scheme_mode", "color_scheme_mode"),
];

/// Rewrite British spelling aliases in one layer to their canonical wire names.
///
/// Serde treats each alias as an alternative name for the same field, so two
/// keys for one field in a single table are a duplicate-field error. Layers are
/// merged as TOML before deserialization, which means a lower layer using the
/// canonical name and a higher layer using the alias would otherwise produce
/// exactly that duplicate and abort the load instead of applying precedence.
/// Normalising each layer before the merge lets the higher layer override the
/// lower one under the canonical key. When a single layer sets both spellings,
/// both keys are kept so deserialization still reports the conflict.
fn normalise_config_aliases(config: &mut TomlValue) {
    let TomlValue::Table(root) = config else {
        return;
    };
    for (table, alias, canonical) in CONFIG_KEY_ALIASES {
        let Some(TomlValue::Table(section)) = root.get_mut(*table) else {
            continue;
        };
        if section.contains_key(*canonical) {
            continue;
        }
        if let Some(value) = section.remove(*alias) {
            section.insert((*canonical).to_owned(), value);
        }
    }
}

/// A single layer of configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLayerEntry {
    /// Source of this layer
    pub source: ConfigLayerSource,
    /// Stable metadata for this layer
    pub metadata: ConfigLayerMetadata,
    /// Parsed TOML content
    pub(crate) config: TomlValue,
    /// Optional reason the layer was disabled
    pub disabled_reason: Option<LayerDisabledReason>,
    /// Optional error attached to this layer
    pub error: Option<ConfigLayerLoadError>,
}

impl ConfigLayerEntry {
    /// Create a new configuration layer entry.
    pub(crate) fn new(source: ConfigLayerSource, mut config: TomlValue) -> Self {
        normalise_config_aliases(&mut config);
        let metadata = ConfigLayerMetadata {
            name: source.label(),
            version: fingerprint_toml_value(&config),
        };
        Self {
            source,
            metadata,
            config,
            disabled_reason: None,
            error: None,
        }
    }

    /// Create a disabled layer entry while retaining layer metadata.
    pub(crate) fn disabled(source: ConfigLayerSource, reason: LayerDisabledReason, message: impl Into<String>) -> Self {
        let message = message.into();
        let config = TomlValue::Table(toml::Table::new());
        let metadata = ConfigLayerMetadata {
            name: source.label(),
            version: fingerprint_toml_value(&TomlValue::String(format!("{}:{}", source.label(), message))),
        };
        Self {
            source,
            metadata,
            config,
            disabled_reason: Some(reason),
            error: Some(ConfigLayerLoadError { message }),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.disabled_reason.is_none() && self.error.is_none()
    }
}

/// A stack of configuration layers, ordered from lowest to highest precedence.
#[derive(Debug, Clone, Default)]
pub struct ConfigLayerStack {
    layers: Vec<ConfigLayerEntry>,
}

impl ConfigLayerStack {
    /// Create a new configuration layer stack.
    pub fn new(layers: Vec<ConfigLayerEntry>) -> Self {
        Self { layers }
    }

    /// Add a layer to the stack.
    pub(crate) fn push(&mut self, layer: ConfigLayerEntry) {
        self.layers.push(layer);
    }

    /// Retain only layers matching the predicate.
    pub(crate) fn retain<F: FnMut(&ConfigLayerEntry) -> bool>(&mut self, mut f: F) {
        self.layers.retain(|layer| f(layer));
    }

    /// Merge all layers into a single effective configuration.
    pub(crate) fn effective_config(&self) -> TomlValue {
        self.effective_config_with_origins().0
    }

    /// Merge all layers and return an origin map (`path -> winning layer metadata`).
    pub fn effective_config_with_origins(&self) -> (TomlValue, HashMap<String, ConfigLayerMetadata>) {
        let mut merged = TomlValue::Table(toml::Table::new());
        let mut origins = HashMap::new();
        for layer in self.ordered_enabled_layers() {
            merge_toml_values_with_origins(&mut merged, &layer.config, &mut origins, &layer.metadata);
        }
        (merged, origins)
    }

    /// Return the first layer error in precedence order.
    pub(crate) fn first_layer_error(&self) -> Option<(&ConfigLayerEntry, &ConfigLayerLoadError)> {
        for layer in self.ordered_layers() {
            if let Some(error) = layer.error.as_ref() {
                return Some((layer, error));
            }
        }
        None
    }

    /// Merge all enabled layers without origin tracking.
    pub fn effective_config_without_origins(&self) -> TomlValue {
        let mut merged = TomlValue::Table(toml::Table::new());
        for layer in self.ordered_enabled_layers() {
            merge_toml_values(&mut merged, &layer.config);
        }
        merged
    }

    fn ordered_layers(&self) -> Vec<&ConfigLayerEntry> {
        let mut with_index: Vec<(usize, &ConfigLayerEntry)> = self.layers.iter().enumerate().collect();
        with_index.sort_by(|(left_idx, left), (right_idx, right)| {
            left.source
                .precedence()
                .cmp(&right.source.precedence())
                .then(left_idx.cmp(right_idx))
        });
        with_index.into_iter().map(|(_, layer)| layer).collect()
    }

    fn ordered_enabled_layers(&self) -> Vec<&ConfigLayerEntry> {
        self.ordered_layers().into_iter().filter(|layer| layer.is_enabled()).collect()
    }

    /// Get all layers in the stack.
    pub fn layers(&self) -> &[ConfigLayerEntry] {
        &self.layers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::DiapasonConfig;
    use crate::root::ColourSchemeMode;

    fn layer(source: ConfigLayerSource, content: &str) -> ConfigLayerEntry {
        ConfigLayerEntry::new(source, toml::from_str(content).expect("valid test toml"))
    }

    fn user_layer(content: &str) -> ConfigLayerEntry {
        layer(
            ConfigLayerSource::User {
                file: PathBuf::from("/home/u/.config/diapason/config.toml"),
            },
            content,
        )
    }

    fn workspace_layer(content: &str) -> ConfigLayerEntry {
        layer(ConfigLayerSource::Workspace { file: PathBuf::from("/ws/config.toml") }, content)
    }

    #[test]
    fn higher_layer_alias_overrides_lower_layer_canonical_key() {
        let mut stack = ConfigLayerStack::default();
        stack.push(user_layer("[ui]\ncolor_scheme_mode = \"dark\"\nsafe_colors_only = false"));
        stack.push(workspace_layer("[ui]\ncolour_scheme_mode = \"light\"\nsafe_colours_only = true"));

        let (effective, origins) = stack.effective_config_with_origins();
        let config: DiapasonConfig = effective.try_into().expect("cross-layer aliases should merge");
        assert_eq!(
            config.ui.colour_scheme_mode,
            ColourSchemeMode::Light,
            "the higher layer's British alias should override the lower layer's canonical key"
        );
        assert!(config.ui.safe_colours_only, "the higher layer's British alias should override the lower layer");
        assert!(
            origins.contains_key("ui.color_scheme_mode"),
            "origins should record the canonical colour-scheme path"
        );
        assert!(origins.contains_key("ui.safe_colors_only"), "origins should record the canonical safe-colours path");
        assert!(!origins.contains_key("ui.colour_scheme_mode"), "origins should not record the alias path");
        assert!(!origins.contains_key("ui.safe_colours_only"), "origins should not record the alias path");

        let without_origins: DiapasonConfig = stack
            .effective_config_without_origins()
            .try_into()
            .expect("cross-layer aliases should merge without origin tracking");
        assert_eq!(without_origins.ui.colour_scheme_mode, ColourSchemeMode::Light);
        assert!(without_origins.ui.safe_colours_only);
    }

    #[test]
    fn alias_only_layer_is_normalised_to_canonical_key() {
        let entry = workspace_layer("[ui]\nsafe_colours_only = true\ncolour_scheme_mode = \"dark\"");
        let ui = entry.config.get("ui").and_then(TomlValue::as_table).expect("ui table");
        assert!(ui.contains_key("safe_colors_only"), "the alias should be rewritten to the canonical key");
        assert!(ui.contains_key("color_scheme_mode"), "the alias should be rewritten to the canonical key");
        assert!(!ui.contains_key("safe_colours_only"), "the alias key should not survive normalisation");
        assert!(!ui.contains_key("colour_scheme_mode"), "the alias key should not survive normalisation");
    }

    #[test]
    fn both_spellings_in_one_layer_remain_a_duplicate_field_error() {
        let mut stack = ConfigLayerStack::default();
        stack.push(workspace_layer("[ui]\nsafe_colors_only = true\nsafe_colours_only = false"));

        let (effective, _) = stack.effective_config_with_origins();
        let result: Result<DiapasonConfig, _> = effective.try_into();
        assert!(result.is_err(), "both spellings in one layer should still be rejected as duplicate aliases");
    }
}
