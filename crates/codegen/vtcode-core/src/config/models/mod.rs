//! Model configuration and identification module
//!
//! This module provides a centralized enum for model identifiers and their configurations,
//! replacing hardcoded model strings throughout the codebase for better maintainability.
//! Read the model list in `docs/models.json`.

#[cfg(test)]
mod tests;

pub use vtcode_config::models::{
    MiMoAuthMethod, ModelCatalogueEntry, ModelId, ModelParseError, ModelPricing, OpenRouterMetadata, Provider,
    ProviderModelSupport, catalogue_provider_keys, detect_mimo_auth_method, model_catalogue_entry,
    supported_models_for_provider,
};
