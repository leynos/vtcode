/// Subcommands for the unified per-session state store.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum SessionStoreCommand {
    /// Migrate legacy `history/` and `logs/` stores into the unified
    /// per-session store. Pass `--remove-legacy` to delete the originals
    /// afterwards.
    Migrate {
        /// Delete the now-imported `history/` and `logs/` directories.
        #[arg(long)]
        remove_legacy: bool,
    },
    /// Apply retention policy, evicting the oldest/stale sessions.
    Gc {
        /// Maximum number of sessions to keep.
        #[arg(long, default_value_t = 50)]
        max_sessions: usize,
        /// Maximum session age in days.
        #[arg(long, default_value_t = 30)]
        max_age_days: u64,
    },
    /// List recent sessions.
    List {
        /// Number of sessions to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Print a session's manifest.
    Inspect {
        /// Session id (directory name under the workspace-specific XDG state sessions directory).
        session: String,
    },
    /// Query grounded facts across all sessions (long-term learning).
    Facts {
        /// Maximum number of facts to return.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}
