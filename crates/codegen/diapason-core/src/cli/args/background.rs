use clap::Args;

/// Arguments for `diapason background-subagent`.
#[derive(Debug, Clone, Args)]
pub struct BackgroundSubagentArgs {
    /// Agent name to run
    #[arg(long = "agent-name", value_name = "NAME")]
    pub agent_name: String,
    /// Parent session ID
    #[arg(long = "parent-session-id", value_name = "SESSION_ID")]
    pub parent_session_id: String,
    /// Session ID for the background subagent
    #[arg(long = "session-id", value_name = "SESSION_ID")]
    pub session_id: String,
    /// Prompt to execute
    #[arg(long = "prompt", value_name = "PROMPT")]
    pub prompt: String,
    /// Maximum number of turns
    #[arg(long = "max-turns", value_name = "COUNT")]
    pub max_turns: Option<usize>,
    /// Override the model for this subagent
    #[arg(long = "model-override", value_name = "MODEL")]
    pub model_override: Option<String>,
    /// Override the reasoning level for this subagent
    #[arg(long = "reasoning-override", value_name = "LEVEL")]
    pub reasoning_override: Option<String>,
}
