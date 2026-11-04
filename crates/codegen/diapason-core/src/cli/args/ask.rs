use clap::ValueEnum;

/// Options for the `ask` command
#[derive(Debug, Default, Clone)]
pub struct AskCommandOptions {
    pub output_format: Option<AskOutputFormat>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub skip_confirmations: bool,
}

/// Output format options for the `ask` subcommand.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AskOutputFormat {
    /// Emit the response as a structured JSON document.
    Json,
}
