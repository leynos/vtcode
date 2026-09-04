//! Tests layered configuration loading, workspace resolution, defaults, and persistence behavior.
use super::*;

use crate::constants::tool_limits;
use crate::core::prompt_cache::PromptCacheRetention;
use crate::core::{
    CustomProviderApiFormat, CustomProviderConfig, CustomProviderPricingConfig, CustomProviderProfileConfig,
    RateLimitHeaderConfig,
};
use crate::defaults::{self, SyntaxHighlightingDefaults, WorkspacePathsDefaults};
use crate::ide_context::{IdeContextProviderConfig, IdeContextProviderMode, IdeContextProvidersConfig};
use crate::loader::layers::ConfigLayerSource;
use crate::types::ReasoningEffortLevel;
use serial_test::serial;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use tempfile::NamedTempFile;
use vtcode_commons::canonicalize;
use vtcode_commons::reference::StaticWorkspacePaths;

mod config_defaults;
mod config_persistence;
mod config_sparse_save;
mod custom_provider_profile_config;
mod custom_provider_trust;
mod ide_context;
mod layered_config;
mod loader_field_parsing;
mod loader_phase_timing;
mod session_overrides;
mod syntax_highlighting;
mod toml_value_merging;
mod workspace_defaults;
