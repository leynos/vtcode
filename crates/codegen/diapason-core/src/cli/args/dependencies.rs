use clap::{Subcommand, ValueEnum};

/// Optional Diapason dependency names
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ManagedDependency {
    #[value(name = "search-tools")]
    SearchTools,
    #[value(name = "ripgrep")]
    Ripgrep,
    #[value(name = "ast-grep")]
    AstGrep,
}

/// Dependency management subcommands
#[derive(Debug, Subcommand, Clone)]
pub enum DependenciesSubcommand {
    /// Install or update an optional dependency
    #[command(name = "install")]
    Install {
        /// Dependency to install
        dependency: ManagedDependency,
    },

    /// Show current status for an optional dependency
    #[command(name = "status")]
    Status {
        /// Dependency to inspect
        dependency: ManagedDependency,
    },
}
