use clap::{Subcommand, ValueEnum};

/// Output format options for the `schema` command.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum SchemaOutputFormat {
    /// Emit one JSON document with all selected schemas.
    Json,
    /// Emit one JSON object per line.
    Ndjson,
}

/// Documentation detail level for the `schema` command.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum SchemaMode {
    /// Minimal descriptions and compact parameter metadata.
    Minimal,
    /// Balanced descriptions for agent discovery.
    Progressive,
    /// Full descriptions and full parameter metadata.
    Full,
}

/// Schema-focused subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum SchemaCommands {
    /// List built-in Diapason tool schemas.
    Tools {
        /// Documentation detail level for tool descriptions.
        #[arg(long, value_enum, default_value_t = SchemaMode::Progressive)]
        mode: SchemaMode,
        /// Output format for schema payloads.
        #[arg(long, value_enum, default_value_t = SchemaOutputFormat::Json)]
        format: SchemaOutputFormat,
        /// Filter by tool name (repeatable).
        #[arg(long = "name", value_name = "TOOL")]
        names: Vec<String>,
    },
}
