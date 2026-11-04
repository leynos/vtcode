use clap::{Args, Subcommand, ValueHint};
use std::path::PathBuf;

/// `schedule` subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum ScheduleSubcommand {
    /// Create a durable scheduled task
    #[command(
        long_about = "Create a durable scheduled task.\n\nExamples:\n  diapason schedule create --prompt \"check the deployment\" --every 10m\n  diapason schedule create --prompt \"review the nightly build\" --cron \"0 9 * * 1-5\"\n  diapason schedule create --reminder \"push the release branch\" --at \"15:00\""
    )]
    Create(ScheduleCreateArgs),
    /// List durable scheduled tasks
    List,
    /// Delete a durable scheduled task by id
    Delete {
        #[arg(value_name = "TASK_ID")]
        id: String,
    },
    /// Run the local durable scheduler daemon
    Serve,
    /// Install the scheduler as a user service
    #[command(name = "install-service")]
    InstallService,
    /// Uninstall the scheduler user service
    #[command(name = "uninstall-service")]
    UninstallService,
}

/// Arguments for `diapason schedule create`.
#[derive(Args, Debug, Clone)]
pub struct ScheduleCreateArgs {
    /// Optional short label for the task
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Prompt to run with `diapason exec`
    #[arg(long, value_name = "PROMPT", conflicts_with = "reminder")]
    pub prompt: Option<String>,
    /// Local reminder text to surface without invoking the model
    #[arg(long, value_name = "TEXT", conflicts_with = "prompt")]
    pub reminder: Option<String>,
    /// Fixed interval such as 10m, 2h, or 1d
    #[arg(long, value_name = "DURATION", conflicts_with_all = ["cron", "at"])]
    pub every: Option<String>,
    /// Five-field cron expression
    #[arg(long, value_name = "EXPR", conflicts_with_all = ["every", "at"])]
    pub cron: Option<String>,
    /// One-shot local time (RFC3339, YYYY-MM-DD HH:MM, or HH:MM)
    #[arg(long, value_name = "TIME", conflicts_with_all = ["every", "cron"])]
    pub at: Option<String>,
    /// Workspace to use for prompt tasks
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub workspace: Option<PathBuf>,
}
