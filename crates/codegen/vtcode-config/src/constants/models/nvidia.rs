//! Curated NVIDIA NIM models exposed by VT Code.

pub const NEMOTRON_3_ULTRA_550B_A55B: &str = "nvidia/nemotron-3-ultra-550b-a55b";
pub const NEMOTRON_3_SUPER_120B_A12B: &str = "nvidia/nemotron-3-super-120b-a12b";
pub const NEMOTRON_3_NANO_30B_A3B: &str = "nvidia/nemotron-3-nano-30b-a3b";
pub const Z_AI_GLM_5_2: &str = "z-ai/glm-5.2";
pub const DEEPSEEK_V4_FLASH_0731: &str = "deepseek-ai/deepseek-v4-flash-0731";

pub const DEFAULT_MODEL: &str = NEMOTRON_3_ULTRA_550B_A55B;

/// Curated agent-oriented NVIDIA models. Explicit model IDs outside this list
/// remain valid because NVIDIA's catalogue is larger than VT Code's picker.
pub const SUPPORTED_MODELS: &[&str] = &[
    NEMOTRON_3_ULTRA_550B_A55B,
    NEMOTRON_3_SUPER_120B_A12B,
    NEMOTRON_3_NANO_30B_A3B,
    Z_AI_GLM_5_2,
    DEEPSEEK_V4_FLASH_0731,
];

pub const REASONING_MODELS: &[&str] = SUPPORTED_MODELS;
