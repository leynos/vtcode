use std::borrow::Cow;

use super::ModelId;

impl ModelId {
    /// Convert the model identifier to its string representation
    /// used in API calls and configurations.
    ///
    /// Returns `Cow<'static, str>` because custom user-defined models
    /// carry runtime strings that may not be `'static`.
    pub fn as_str(&self) -> Cow<'static, str> {
        if let Some(meta) = self.openrouter_metadata() {
            return Cow::Borrowed(meta.id);
        }
        if let Some(id) = self.table_id() {
            return Cow::Borrowed(id);
        }
        match self {
            // OpenRouter models without generated metadata
            ModelId::OpenRouterMoonshotaiKimiK3 => Cow::Borrowed("moonshotai/kimi-k3"),
            ModelId::OpenRouterMoonshotaiKimiK27Code => Cow::Borrowed("moonshotai/kimi-k2.7-code"),
            ModelId::OpenRouterZaiGlm52 => Cow::Borrowed("z-ai/glm-5.2"),
            ModelId::OpenRouterZaiGlm53Flash => Cow::Borrowed("z-ai/glm-5.3-flash"),
            // Custom user-defined models
            ModelId::Custom(_, model) => Cow::Owned(model.clone()),
            _ => unreachable!("built-in model missing generated or table metadata"),
        }
    }
}
