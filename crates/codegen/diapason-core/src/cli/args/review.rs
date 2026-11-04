use clap::{Args, ValueHint};
use std::path::PathBuf;

/// Arguments for `diapason review`.
#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    /// Emit structured JSON events to stdout (one per line)
    #[arg(long)]
    pub json: bool,
    /// Optional path to write the JSONL transcript
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub events: Option<PathBuf>,
    /// Write the last agent message to this file
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub last_message_file: Option<PathBuf>,
    /// Review the last committed diff instead of the current diff
    #[arg(long, conflicts_with_all = ["target", "files"])]
    pub last_diff: bool,
    /// Review a custom git target expression
    #[arg(long, value_name = "TARGET", conflicts_with = "files")]
    pub target: Option<String>,
    /// Optional review style or focus area
    #[arg(long, value_name = "STYLE")]
    pub style: Option<String>,
    /// Review specific files instead of a diff target (repeatable)
    #[arg(long, value_name = "FILE", conflicts_with = "target")]
    pub files: Vec<PathBuf>,
}
