use super::openai;

pub const DEFAULT_MODEL: &str = openai::DEFAULT_MODEL;
pub const SUPPORTED_MODELS: &[&str] = openai::RESPONSES_API_MODELS;

pub const GPT: &str = openai::GPT;
pub const GPT_5: &str = openai::GPT_5;
pub const GPT_5_6: &str = openai::GPT_5_6;
pub const GPT_5_6_SOL: &str = openai::GPT_5_6_SOL;
pub const GPT_5_MINI: &str = openai::GPT_5_MINI;
pub const GPT_5_NANO: &str = openai::GPT_5_NANO;
pub const GPT_5_CODEX: &str = openai::GPT_5_CODEX;
pub const O3: &str = openai::O3;
pub const O4_MINI: &str = openai::O4_MINI;
