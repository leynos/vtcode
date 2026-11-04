// OpenCode Zen models (pay-as-you-go gateway)
// https://opencode.ai/docs/zen/
pub const DEFAULT_MODEL: &str = GPT_5_6_SOL;

pub const GPT_5_6_SOL: &str = "gpt-5.6-sol";
pub(crate) const GPT_5_6_LUNA: &str = "gpt-5.6-luna";
const GPT_5_CODEX: &str = "gpt-5-codex";
pub(crate) const GPT_5_6: &str = "gpt-5.6";
const GPT_5_1: &str = "gpt-5.1";
const GPT_5_1_CODEX: &str = "gpt-5.1-codex";
const GPT_5_1_CODEX_MAX: &str = "gpt-5.1-codex-max";
const GPT_5: &str = "gpt-5";
const GPT_5_NANO: &str = "gpt-5-nano";

const CLAUDE_OPUS_4_5: &str = "claude-opus-4-5";
const CLAUDE_OPUS_4_1: &str = "claude-opus-4-1";
pub(crate) const CLAUDE_SONNET_5: &str = "claude-sonnet-5";
const CLAUDE_SONNET_4_5: &str = "claude-sonnet-4-5";
const CLAUDE_SONNET_4: &str = "claude-sonnet-4";
const CLAUDE_3_5_HAIKU: &str = "claude-3-5-haiku";

const MINIMAX_M3: &str = "minimax-m2.5";
const MINIMAX_M2_5_FREE: &str = "minimax-m2.5-free";
const KIMI_K2_5: &str = "kimi-k2.5";
const BIG_PICKLE: &str = "big-pickle";

pub const OPENAI_MODELS: &[&str] = &[
    GPT_5_6_SOL,
    GPT_5_6_SOL,
    GPT_5_6_LUNA,
    GPT_5_6_LUNA,
    GPT_5_CODEX,
    GPT_5_6,
    GPT_5_CODEX,
    GPT_5_1,
    GPT_5_1_CODEX,
    GPT_5_1_CODEX_MAX,
    GPT_5,
    GPT_5_CODEX,
    GPT_5_NANO,
];

pub const ANTHROPIC_MODELS: &[&str] = &[
    CLAUDE_OPUS_4_5,
    CLAUDE_OPUS_4_1,
    CLAUDE_SONNET_5,
    CLAUDE_SONNET_4_5,
    CLAUDE_SONNET_4,
    CLAUDE_SONNET_5,
    CLAUDE_3_5_HAIKU,
];

pub const OPENAI_COMPATIBLE_MODELS: &[&str] = &[MINIMAX_M3, MINIMAX_M2_5_FREE, KIMI_K2_5, BIG_PICKLE];

// Curated models Diapason currently exposes in config flows and ModelId metadata.
pub(crate) const CONFIGURED_MODELS: &[&str] = &[GPT_5_6_SOL, GPT_5_6_LUNA, CLAUDE_SONNET_5, KIMI_K2_5];

pub const SUPPORTED_MODELS: &[&str] = &[
    GPT_5_6_SOL,
    GPT_5_6_SOL,
    GPT_5_6_LUNA,
    GPT_5_6_LUNA,
    GPT_5_CODEX,
    GPT_5_6,
    GPT_5_CODEX,
    GPT_5_1,
    GPT_5_1_CODEX,
    GPT_5_1_CODEX_MAX,
    GPT_5,
    GPT_5_CODEX,
    GPT_5_NANO,
    CLAUDE_OPUS_4_5,
    CLAUDE_OPUS_4_1,
    CLAUDE_SONNET_5,
    CLAUDE_SONNET_4_5,
    CLAUDE_SONNET_4,
    CLAUDE_SONNET_5,
    CLAUDE_3_5_HAIKU,
    MINIMAX_M3,
    MINIMAX_M2_5_FREE,
    KIMI_K2_5,
    BIG_PICKLE,
];
pub const REASONING_MODELS: &[&str] = &[];
