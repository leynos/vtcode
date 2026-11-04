pub(crate) const GROK_BUILD_0_1: &str = "grok-build-0.1";
pub(crate) const GROK_4_6: &str = "grok-4.6";
pub(crate) const GROK_4_20_REASONING: &str = "grok-4.20-0309-reasoning";
const GROK_4_20_NON_REASONING: &str = "grok-4.20-0309-non-reasoning";
const GROK_4_20_MULTI_AGENT: &str = "grok-4.20-multi-agent-0309";

pub const DEFAULT_MODEL: &str = GROK_4_6;

pub const SUPPORTED_MODELS: &[&str] = &[
    GROK_4_6,
    GROK_4_20_REASONING,
    GROK_4_20_NON_REASONING,
    GROK_BUILD_0_1,
    GROK_4_20_MULTI_AGENT,
];

pub const REASONING_MODELS: &[&str] = &[GROK_4_6, GROK_4_20_REASONING];
