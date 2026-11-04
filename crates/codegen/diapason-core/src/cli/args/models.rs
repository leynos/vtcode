use clap::Subcommand;

/// Model management commands with concise, actionable help
#[derive(Subcommand, Debug, Clone)]
pub enum ModelCommands {
    /// List all providers and models with status indicators
    List,

    /// Set default provider (gemini, openai, anthropic, deepseek, meta, openrouter, merge-gateway)
    #[command(name = "set-provider")]
    SetProvider {
        /// Provider name to set as default
        provider: String,
    },

    /// Set default model (e.g., deepseek-reasoner, gpt-5, claude-sonnet-4-6)
    #[command(name = "set-model")]
    SetModel {
        /// Model name to set as default
        model: String,
    },

    /// Configure provider settings (API keys, base URLs, models)
    Config {
        /// Provider name to configure
        provider: String,

        /// API key for the provider
        #[arg(long)]
        api_key: Option<String>,

        /// Base URL for local providers
        #[arg(long)]
        base_url: Option<String>,

        /// Default model for this provider
        #[arg(long)]
        model: Option<String>,
    },

    /// Test provider connectivity and validate configuration
    Test {
        /// Provider name to test
        provider: String,
    },

    /// Compare model performance across providers (coming soon)
    Compare,

    /// Show detailed model information and specifications
    Info {
        /// Model name to get information about
        model: String,
    },
}
