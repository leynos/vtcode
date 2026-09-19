use std::borrow::Cow;

use super::ModelId;

impl ModelId {
    /// Get the display name for the model (human-readable).
    ///
    /// Returns `Cow<'static, str>` because custom user-defined models
    /// carry runtime strings that may not be `'static`.
    pub fn display_name(&self) -> Cow<'static, str> {
        if let Some(meta) = self.openrouter_metadata() {
            return Cow::Borrowed(meta.display);
        }
        if let Some(display) = self.table_display() {
            return Cow::Borrowed(display);
        }
        match self {
            // OpenRouter models without generated metadata
            ModelId::OpenRouterMoonshotaiKimiK3 => Cow::Borrowed("Kimi K3 (OpenRouter)"),
            ModelId::OpenRouterMoonshotaiKimiK27Code => Cow::Borrowed("Kimi K2.7 Code (OpenRouter)"),
            ModelId::OpenRouterZaiGlm52 => Cow::Borrowed("GLM-5.2 (OpenRouter)"),
            ModelId::OpenRouterZaiGlm53Flash => Cow::Borrowed("GLM-5.3 Flash"),
            // Custom user-defined models
            ModelId::Custom(_, model) => Cow::Owned(model.clone()),
            _ => unreachable!("built-in model missing generated or table metadata"),
        }
    }
}
