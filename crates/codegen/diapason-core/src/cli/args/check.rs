use clap::Subcommand;

/// Built-in repository checks
#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub enum CheckSubcommand {
    /// Run ast-grep rule tests and scan for the current workspace
    #[command(name = "ast-grep")]
    AstGrep,
}
