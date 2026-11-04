use clap::Subcommand;
use std::path::PathBuf;

/// Agent Plugins subcommands
#[derive(Debug, Subcommand, Clone)]
pub enum PluginsSubcommand {
    /// List installed Agent Plugins
    #[command(name = "list")]
    List,

    /// Show plugin details
    #[command(name = "info")]
    Info {
        /// Plugin name or path
        name: String,
    },

    /// Validate a plugin directory
    #[command(name = "validate")]
    Validate {
        /// Path to plugin directory
        path: PathBuf,
    },

    /// Install a plugin from a git URL
    #[command(name = "add")]
    Add {
        /// Git URL or local path to plugin
        source: String,
        /// Plugin name (defaults to directory name)
        #[arg(long)]
        name: Option<String>,
    },

    /// Remove an installed plugin
    #[command(name = "remove")]
    Remove {
        /// Plugin name to remove
        name: String,
    },
}
