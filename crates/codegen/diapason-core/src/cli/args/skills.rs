use clap::Subcommand;

/// Skills subcommands
#[derive(Debug, Subcommand, Clone)]
pub enum SkillsSubcommand {
    /// List available skills
    #[command(name = "list")]
    List {
        /// Show all skills including system skills
        #[arg(long)]
        all: bool,
    },

    /// Load a skill for use in agent session
    #[command(name = "load")]
    Load {
        /// Skill name to load
        name: String,
        /// Optional path to skill directory
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Unload a skill from session
    #[command(name = "unload")]
    Unload {
        /// Skill name to unload
        name: String,
    },

    /// Show skill details and instructions
    #[command(name = "info")]
    Info {
        /// Skill name to get information about
        name: String,
    },

    /// Create a new skill from template
    #[command(name = "create")]
    Create {
        /// Path for new skill directory
        path: std::path::PathBuf,
        /// Optional template to use
        #[arg(long)]
        template: Option<String>,
    },

    /// Validate SKILL.md manifest
    #[command(name = "validate")]
    Validate {
        /// Path to skill directory or SKILL.md file
        path: std::path::PathBuf,
        /// Enable strict validation (warnings become errors for routing quality checks)
        #[arg(long)]
        strict: bool,
    },

    /// Validate all skills for container skills compatibility
    #[command(name = "check-compatibility")]
    CheckCompatibility,

    /// Show skill configuration and search paths
    #[command(name = "config")]
    Config,

    /// Regenerate skills index file
    #[command(name = "regenerate-index")]
    RegenerateIndex,

    /// skills-ref compatible commands (agentskills.io spec)
    #[command(name = "skills-ref", subcommand)]
    SkillsRef(SkillsRefSubcommand),
}

/// skills-ref compatible subcommands per agentskills.io specification
#[derive(Debug, Subcommand, Clone)]
pub enum SkillsRefSubcommand {
    /// Validate a skill directory
    #[command(name = "validate")]
    Validate {
        /// Path to skill directory
        path: std::path::PathBuf,
    },

    /// Generate <available_skills> XML for agent prompts
    #[command(name = "to-prompt")]
    ToPrompt {
        /// Paths to skill directories
        paths: Vec<std::path::PathBuf>,
    },

    /// List discovered skills
    #[command(name = "list")]
    List {
        /// Optional path to search (defaults to current directory)
        path: Option<std::path::PathBuf>,
    },
}
