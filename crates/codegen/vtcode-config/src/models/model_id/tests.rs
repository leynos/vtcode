use super::ModelId;

#[test]
fn openrouter_glm53_flash_metadata_matches_its_manual_accessors() {
    let model = ModelId::OpenRouterZaiGlm53Flash;

    assert_eq!(model.as_str(), "z-ai/glm-5.3-flash");
    assert_eq!(model.display_name(), "GLM-5.3 Flash");
    assert_eq!(model.description(), "Z.AI GLM-5.3 Flash efficient multimodal model via OpenRouter");
}
