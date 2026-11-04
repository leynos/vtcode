use clap::{Args, Subcommand};

/// `exec` subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum ExecSubcommand {
    /// Resume a previous exec session with a follow-up prompt
    #[command(
        long_about = "Resume a previous exec session with a follow-up prompt.\n\nExamples:\n  diapason exec resume session-123 \"continue from the prior investigation\"\n  diapason exec resume --last \"continue from the prior investigation\"\n  echo \"continue from stdin\" | diapason exec resume --last"
    )]
    Resume(ExecResumeArgs),
    /// Run an evaluation suite for regression or capability testing
    #[command(
        long_about = "Run an evaluation suite against the agent. Each task in the suite is\n\
                      executed autonomously, then verified with environment probes.\n\
                      Results are aggregated into a report with pass@k and pass^k metrics.\n\n\
                      Examples:\n  diapason exec eval --suite my-suite.json\n  diapason exec eval --suite suite.json --output report.md"
    )]
    Eval(ExecEvalArgs),
}

/// Arguments for `diapason exec eval`.
#[derive(Args, Debug, Clone)]
pub struct ExecEvalArgs {
    /// Path to the eval suite JSON file
    #[arg(long, value_name = "FILE")]
    pub suite: String,
    /// Optional path to write the markdown report
    #[arg(long, value_name = "FILE")]
    pub output: Option<String>,
}

/// Arguments for `diapason exec resume`.
#[derive(Args, Debug, Clone)]
pub struct ExecResumeArgs {
    /// Resume the most recent archived exec session
    #[arg(long)]
    pub last: bool,
    /// Search archived exec sessions across every workspace
    #[arg(long)]
    pub all: bool,
    /// Archived session identifier to resume, or the prompt when `--last` is used
    #[arg(value_name = "SESSION_ID_OR_PROMPT", required_unless_present = "last")]
    pub session_or_prompt: Option<String>,
    /// Follow-up prompt to execute when resuming a specific session. Use `-` to force reading from stdin.
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,
}
