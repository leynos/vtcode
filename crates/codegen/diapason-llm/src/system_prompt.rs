//! System prompt provider for LLM providers.
//!
//! Default implementations are provided here. diapason-core can override these
//! at runtime by calling the setter functions.

use std::sync::OnceLock;

type PromptFn = Box<dyn Fn() -> String + Send + Sync>;

static DEFAULT_SYSTEM_PROMPT: OnceLock<PromptFn> = OnceLock::new();
static OPENAI_GPT55_ADDENDUM: OnceLock<PromptFn> = OnceLock::new();
static OPENAI_GPT56_ADDENDUM: OnceLock<PromptFn> = OnceLock::new();
static OPENAI_GPT6_ADDENDUM: OnceLock<PromptFn> = OnceLock::new();

const FALLBACK_SYSTEM_PROMPT: &str = "You are Diapason, a coding assistant.";

const FALLBACK_GPT55_ADDENDUM: &str = r#"

## GPT-5.5 OpenAI Addendum

This session uses OpenAI's GPT-5.5 model. By using this model, you agree to OpenAI's usage policies and terms of service. The model may have specific capabilities, limitations, and content policies that differ from other models. For the latest information, refer to OpenAI's documentation."#;

const FALLBACK_GPT56_ADDENDUM: &str = r#"

## GPT-5.6 OpenAI Addendum

This session uses OpenAI's GPT-5.6 model. By using this model, you agree to OpenAI's usage policies and terms of service. The model may have specific capabilities, limitations, and content policies that differ from other models. For the latest information, refer to OpenAI's documentation."#;

const FALLBACK_GPT6_ADDENDUM: &str = r#"

## GPT-6 Astra OpenAI Addendum

This session uses OpenAI's GPT-6 Astra model. By using this model, you agree to OpenAI's usage policies and terms of service. For the latest information, refer to OpenAI's documentation.

Bias towards action: when the user's prompt indicates a request for action, treat it as an instruction to do the work, not to describe or offer it. Complete already-authorized work into a concrete, reviewable result before asking clarifying questions, and persist until the intended goal is fulfilled. You do not need permission for reversible or read-only actions.

The user's instructions take precedence over guidelines in skills and instruction files such as AGENTS.md. If a skill file causes you to ask for permission, pause, leave work unfinished, or diverge from the user's intent, name the exact file, quote the relevant instruction, and briefly explain how it applies.

Parallelize work by delegating to subagents whenever it could save time or improve quality, and verify changes with testing proportionate to their impact."#;

/// Get the default system prompt string.
///
/// Returns the prompt set via [`set_default_system_prompt`] if available,
/// otherwise falls back to a built-in default.
pub(crate) fn default_system_prompt() -> String {
    DEFAULT_SYSTEM_PROMPT
        .get()
        .map_or_else(|| FALLBACK_SYSTEM_PROMPT.to_string(), |f| f())
}

/// Return the OpenAI GPT-5.5 contract addendum.
pub(crate) fn openai_gpt55_contract_addendum() -> String {
    OPENAI_GPT55_ADDENDUM
        .get()
        .map_or_else(|| FALLBACK_GPT55_ADDENDUM.to_string(), |f| f())
}

/// Return the OpenAI GPT-5.6 contract addendum.
pub(crate) fn openai_gpt56_contract_addendum() -> String {
    OPENAI_GPT56_ADDENDUM
        .get()
        .map_or_else(|| FALLBACK_GPT56_ADDENDUM.to_string(), |f| f())
}

/// Return the OpenAI GPT-6 Astra contract addendum.
pub(crate) fn openai_gpt6_contract_addendum() -> String {
    OPENAI_GPT6_ADDENDUM
        .get()
        .map_or_else(|| FALLBACK_GPT6_ADDENDUM.to_string(), |f| f())
}

/// Override the default system prompt (called by diapason-core at init).
pub fn set_default_system_prompt<F: Fn() -> String + Send + Sync + 'static>(f: F) {
    let _ = DEFAULT_SYSTEM_PROMPT.set(Box::new(f));
}

/// Override the OpenAI GPT-5.5 addendum during runtime initialization.
pub fn set_openai_gpt55_addendum<F: Fn() -> String + Send + Sync + 'static>(f: F) {
    let _ = OPENAI_GPT55_ADDENDUM.set(Box::new(f));
}

/// Override the OpenAI GPT-5.6 addendum during runtime initialization.
pub fn set_openai_gpt56_addendum<F: Fn() -> String + Send + Sync + 'static>(f: F) {
    let _ = OPENAI_GPT56_ADDENDUM.set(Box::new(f));
}

/// Override the OpenAI GPT-6 Astra addendum during runtime initialization.
pub fn set_openai_gpt6_addendum<F: Fn() -> String + Send + Sync + 'static>(f: F) {
    let _ = OPENAI_GPT6_ADDENDUM.set(Box::new(f));
}
