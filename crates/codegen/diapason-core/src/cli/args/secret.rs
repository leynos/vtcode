use clap::{Args, Subcommand, ValueEnum};

/// Supported built-in provider names for legacy callers.
///
/// The CLI accepts arbitrary configured provider names through `String`
/// fields below; this enum remains available to integrations that used the
/// older typed API.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SecretProvider {
    #[value(name = "openai")]
    OpenAI,
    #[value(name = "anthropic")]
    Anthropic,
    #[value(name = "gemini")]
    Gemini,
    #[value(name = "deepseek")]
    DeepSeek,
    #[value(name = "meta")]
    Meta,
    #[value(name = "openrouter")]
    OpenRouter,
    #[value(name = "stepfun")]
    StepFun,
    #[value(name = "zai")]
    Zai,
    #[value(name = "moonshot")]
    Moonshot,
    #[value(name = "minimax")]
    MiniMax,
    #[value(name = "mistral")]
    Mistral,
    #[value(name = "huggingface")]
    HuggingFace,
    #[value(name = "mimo")]
    MiMo,
    #[value(name = "opencode-zen")]
    OpenCodeZen,
    #[value(name = "opencode-go")]
    OpenCodeGo,
    #[value(name = "qwen")]
    Qwen,
    #[value(name = "evolink")]
    Evolink,
    #[value(name = "poolside")]
    Poolside,
    #[value(name = "ollama")]
    Ollama,
    #[value(name = "ollama-cloud")]
    OllamaCloud,
    #[value(name = "lmstudio")]
    LMStudio,
    #[value(name = "copilot")]
    Copilot,
    #[value(name = "nvidia")]
    Nvidia,
    #[value(name = "merge-gateway")]
    MergeGateway,
}

/// Secret management subcommands
#[derive(Debug, Subcommand, Clone)]
pub enum SecretSubcommand {
    /// List secret status for all providers
    #[command(name = "list", visible_alias = "ls")]
    List,

    /// Show status for a specific provider
    #[command(name = "status", visible_alias = "info")]
    Status {
        /// Provider name (e.g. openai, anthropic, stepfun)
        provider_name: Option<String>,
        /// Explicit environment-variable identity for a non-default key
        #[arg(long, alias = "env")]
        key_name: Option<String>,
    },

    /// Store an API key in secure storage
    #[command(name = "add", visible_alias = "set")]
    Add {
        /// Provider name (e.g. openai, anthropic, stepfun)
        provider_name: String,
        /// Explicit environment-variable identity for a non-default key
        #[arg(long, alias = "env")]
        key_name: Option<String>,
    },

    /// Remove a stored API key from secure storage
    #[command(name = "delete", visible_alias = "remove")]
    Delete {
        /// Provider name (e.g. openai, anthropic, stepfun)
        provider_name: String,
        /// Explicit environment-variable identity for a non-default key
        #[arg(long, alias = "env")]
        key_name: Option<String>,
    },

    /// Migrate API keys from workspace .env to secure storage
    #[command(name = "migrate")]
    Migrate(MigrateArgs),
}

/// Arguments for `diapason secret migrate`.
#[derive(Debug, Args, Clone)]
pub struct MigrateArgs {
    /// Provider name (e.g. openai, anthropic). Omit to migrate all found keys.
    pub provider_name: Option<String>,

    /// Migrate all found keys without prompting
    #[arg(long)]
    pub all: bool,

    /// Preview migration without making changes
    #[arg(long)]
    pub dry_run: bool,

    /// Skip confirmation prompts
    #[arg(long)]
    pub force: bool,
}

/// Top-level secret command args (allows bare `diapason secret` to default to list)
#[derive(Debug, Args, Clone)]
pub struct SecretArgs {
    #[command(subcommand)]
    pub command: Option<SecretSubcommand>,
}
