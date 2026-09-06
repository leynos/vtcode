//! Man page generation for VT Code CLI using roff-rs
//!
//! This module provides functionality to generate Unix man pages for VT Code
//! commands and subcommands using the roff-rs library.

use anyhow::{Result, bail};
use roff::{Roff, bold, italic, roman};

/// Man page generator for VT Code CLI
pub struct ManPageGenerator;

impl ManPageGenerator {
    /// Get current date in YYYY-MM-DD format
    fn current_date() -> String {
        use chrono::Utc;
        Utc::now().format("%Y-%m-%d").to_string()
    }

    /// Generate man page for the main VT Code command
    pub fn generate_main_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode - Advanced coding agent with Decision Ledger")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] ["),
                bold("COMMAND"),
                roman("] ["),
                bold("ARGS"),
                roman("]"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("VT Code is an advanced coding agent with single-agent architecture and Decision Ledger that provides"),
                roman(" intelligent code generation, analysis, and modification capabilities. It supports"),
                roman(" multiple LLM providers including Gemini, OpenAI, Anthropic, DeepSeek, Meta AI, Z.AI,"),
                roman(" Moonshot AI, OpenRouter, Merge Gateway, NVIDIA NIM, and Ollama, and includes LLM-native semantic code understanding."),
                roman(" Rust, Python, JavaScript, TypeScript, Go, and Java."),
            ])
            .control("SH", ["OPTIONS"])
            .control("TP", [])
            .text([bold("-m"), roman(", "), bold("--model"), roman(" "), italic("MODEL")])
            .text([roman("Specify the LLM model to use (default: gemini-3-flash-preview)")])
            .control("TP", [])
            .text([bold("-p"), roman(", "), bold("--provider"), roman(" "), italic("PROVIDER")])
            .text([
                roman(
                    "Specify the LLM provider (gemini, openai, anthropic, deepseek, meta, zai, moonshot, openrouter, merge-gateway, nvidia, ollama, lmstudio)",
                ),
            ])
            .control("TP", [])
            .text([bold("--workspace"), roman(" "), italic("PATH")])
            .text([roman("Set the workspace root directory for file operations")])
            .control("TP", [])
            .text([bold("--performance-monitoring")])
            .text([roman("Enable performance monitoring and metrics")])
            .control("TP", [])
            .text([bold("--research-preview")])
            .text([roman("Enable research-preview features")])
            .control("TP", [])
            .text([bold("--debug")])
            .text([roman("Enable debug output")])
            .control("TP", [])
            .text([bold("--verbose")])
            .text([roman("Enable verbose logging")])
            .control("TP", [])
            .text([bold("-h"), roman(", "), bold("--help")])
            .text([roman("Display help information")])
            .control("TP", [])
            .text([bold("-V"), roman(", "), bold("--version")])
            .text([roman("Display version information")])
            .control("SH", ["COMMANDS"])
            .control("TP", [])
            .text([bold("chat")])
            .text([roman("Start interactive AI coding assistant")])
            .control("TP", [])
            .text([bold("ask"), roman(" "), italic("PROMPT")])
            .text([roman("Single prompt mode without tools")])
            .control("TP", [])
            .text([bold("performance")])
            .text([roman("Display performance metrics and system status")])
            .control("TP", [])
            .text([bold("benchmark")])
            .text([roman("Run SWE-bench evaluation framework")])
            .control("TP", [])
            .text([bold("create-project"), roman(" "), italic("NAME"), roman(" "), italic("FEATURES")])
            .text([roman("Create complete Rust project with features")])
            .control("TP", [])
            .text([bold("init")])
            .text([roman("Guided AGENTS.md and workspace setup")])
            .control("TP", [])
            .text([bold("man"), roman(" "), italic("COMMAND")])
            .text([roman("Generate or display man pages for commands")])
            .control("TP", [])
            .text([bold("check"), roman(" "), italic("SUBCOMMAND")])
            .text([roman("Run built-in repository checks")])
            .control("TP", [])
            .text([bold("acp")])
            .text([roman("Start Agent Client Protocol bridge for IDE integrations")])
            .control("TP", [])
            .text([bold("chat-verbose")])
            .text([roman("Verbose interactive chat with enhanced transparency")])
            .control("TP", [])
            .text([bold("performance")])
            .text([roman("Display performance metrics and system status")])
            .control("TP", [])
            .text([bold("trajectory")])
            .text([roman("Pretty-print trajectory logs and show basic analytics")])
            .control("TP", [])
            .text([bold("benchmark")])
            .text([roman("Benchmark against SWE-bench evaluation framework")])
            .control("TP", [])
            .text([bold("create-project"), roman(" "), italic("name"), roman(" "), italic("features")])
            .text([roman("Create complete Rust project with advanced features")])
            .control("TP", [])

            .control("TP", [])
            .text([bold("revert"), roman(" "), italic("turn")])
            .text([roman("Revert agent to a previous snapshot")])
            .control("TP", [])
            .text([bold("snapshots")])
            .text([roman("List all available snapshots")])
            .control("TP", [])
            .text([bold("cleanup-snapshots")])
            .text([roman("Clean up old snapshots")])
            .control("TP", [])
            .text([bold("init")])
            .text([roman("Initialize project with enhanced dot-folder structure")])
            .control("TP", [])
            .text([bold("init-project")])
            .text([roman("Initialize project with dot-folder structure")])
            .control("TP", [])
            .text([bold("config")])
            .text([roman("Generate configuration file")])
            .control("TP", [])
            .text([bold("tool-policy")])
            .text([roman("Manage tool execution policies")])
            .control("TP", [])
            .text([bold("mcp")])
            .text([roman("Manage Model Context Protocol providers")])
            .control("TP", [])
            .text([bold("models")])
            .text([roman("Manage models and providers")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Start interactive chat:")])
            .text([bold("  vtcode chat")])
            .text([roman("Ask a question:")])
            .text([bold("  vtcode ask \"Explain Rust ownership\"")])
            .text([roman("Create a web project:")])
            .text([bold("  vtcode create-project myapp web,auth,db")])
            .text([roman("Generate man page:")])
            .text([bold("  vtcode man chat")])
            .text([roman("Run ast-grep checks for the current workspace:")])
            .text([bold("  vtcode check ast-grep")])
            .control("SH", ["ENVIRONMENT"])
            .control("TP", [])
            .text([bold("GEMINI_API_KEY")])
            .text([roman("API key for Google Gemini (default provider)")])
            .control("TP", [])
            .text([bold("OPENAI_API_KEY")])
            .text([roman("API key for OpenAI GPT models")])
            .control("TP", [])
            .text([bold("ANTHROPIC_API_KEY")])
            .text([roman("API key for Anthropic Claude models")])
            .control("TP", [])
            .text([bold("DEEPSEEK_API_KEY")])
            .text([roman("API key for DeepSeek models")])
            .control("TP", [])
            .text([bold("META_API_KEY")])
            .text([roman("API key for Meta AI Muse models")])
            .control("TP", [])
            .text([bold("MODEL_API_KEY")])
            .text([roman("Meta AI's documented API key variable")])
            .control("TP", [])
            .text([bold("ZAI_API_KEY")])
            .text([roman("API key for Z.AI GLM models")])
            .control("TP", [])
            .text([bold("MOONSHOT_API_KEY")])
            .text([roman("API key for Moonshot AI Kimi models")])
            .control("TP", [])
            .text([bold("OPENROUTER_API_KEY")])
            .text([roman("API key for OpenRouter models")])
            .control("TP", [])
            .text([bold("NVIDIA_API_KEY")])
            .text([roman("API key for NVIDIA NIM models")])
            .control("TP", [])
            .text([bold("MERGE_GATEWAY_API_KEY")])
            .text([roman("API key for Merge Gateway routes")])
            .control("TP", [])
            .text([bold("MERGE_GATEWAY_BASE_URL")])
            .text([roman("Optional Merge Gateway endpoint override; /v1/openai selects legacy compatibility")])
            .control("SH", ["FILES"])
            .control("TP", [])
            .text([bold("vtcode.toml")])
            .text([roman("Configuration file (current directory or the canonical user config directory)")])
            .control("TP", [])
            .text([bold(".vtcode/")])
            .text([roman("Project cache and context directory")])
            .control("SH", ["SAFETY"])
            .control("TP", [])
            .text([roman(
                "apply_patch: reserve for reviewed diffs or small batches. For large refactors or critical files, stage local backups and prefer edit_file/write_file to avoid partial rewrites if a patch fails.",
            )])
            .control("TP", [])
            .text([roman(
                "Timeout governance: tune [timeouts] in vtcode.toml to clamp tool duration. VT Code warns once execution passes the configured warning threshold so you can cancel runaway commands.",
            )])
            .control("SH", ["SEE ALSO"])
            .text([roman("Full documentation: https://github.com/vinhnx/vtcode")])
            .text([roman("Related commands: cargo(1), rustc(1), git(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for a specific command
    pub fn generate_command_man_page(command: &str) -> Result<String> {
        match command {
            "chat" => Self::generate_chat_man_page(),
            "ask" => Self::generate_ask_man_page(),
            "performance" => Self::generate_performance_man_page(),
            "benchmark" => Self::generate_benchmark_man_page(),
            "check" => Self::generate_check_man_page(),
            "create-project" => Self::generate_create_project_man_page(),
            "init" => Self::generate_init_man_page(),
            "man" => Self::generate_man_man_page(),
            "analyse" | "analyze" => Self::generate_analyse_man_page(),
            _ => bail!("Unknown command: {command}"),
        }
    }

    /// Generate man page for the chat command
    fn generate_chat_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-CHAT", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-chat - Interactive AI coding assistant")])
            .control("SH", ["SYNOPSIS"])
            .text([bold("vtcode"), roman(" ["), bold("OPTIONS"), roman("] "), bold("chat")])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Start an interactive AI coding assistant session."),
                roman(" The chat command provides intelligent code generation, analysis, and modification"),
                roman(" with support for multiple LLM providers and semantic code analysis."),
            ])
            .control("SH", ["OPTIONS"])
            .text([
                roman("All global options are supported. See "),
                bold("vtcode(1)"),
                roman(" for details."),
            ])
            .control("SH", ["EXAMPLES"])
            .text([roman("Start basic chat session:")])
            .text([bold("  vtcode chat")])
            .text([roman("Start with specific model:")])
            .text([bold("  vtcode --model gemini-3.1-pro-preview chat")])
            .control("SH", ["SEE ALSO"])
            .text([
                bold("vtcode(1)"),
                roman(", "),
                bold("vtcode-ask(1)"),
                roman(", "),
                bold("vtcode-analyse(1)"),
            ])
            .render();

        Ok(page)
    }

    /// Generate man page for the ask command
    fn generate_ask_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-ASK", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-ask - Single prompt mode without tools")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("ask"),
                roman(" "),
                italic("PROMPT"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Execute a single prompt without tool usage. This is perfect for quick questions,"),
                roman(" code explanations, and simple queries that don't require file operations or"),
                roman(" complex tool interactions."),
            ])
            .control("SH", ["EXAMPLES"])
            .text([roman("Ask about Rust ownership:")])
            .text([bold("  vtcode ask \"Explain Rust ownership\"")])
            .text([roman("Get code explanation:")])
            .text([bold("  vtcode ask \"What does this regex do: \\w+@\\w+\\.\\w+\"")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("vtcode-chat(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for the analyse command
    fn generate_analyse_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-ANALYSE", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-analyse - Analyse workspace")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("analyse"),
                roman(" ["),
                italic("TYPE"),
                roman("]"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Analyse the current workspace and report its structure, configuration, and source layout."),
                roman(" The optional analysis mode selects the read-only inspection to perform."),
            ])
            .control("SH", ["ARGUMENTS"])
            .control("TP", [])
            .text([bold("TYPE")])
            .text([roman("Optional analysis mode for the workspace inspection.")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Analyse the whole workspace:")])
            .text([bold("  vtcode analyse")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("vtcode-man(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for the performance command
    fn generate_performance_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-PERFORMANCE", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman(
                "vtcode-performance - Display performance metrics and system status",
            )])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("performance"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Display comprehensive performance metrics and system status information."),
                roman(" Shows token usage, API costs, response times, tool execution statistics,"),
                roman(" memory usage patterns, and agent performance metrics."),
            ])
            .control("SH", ["METRICS DISPLAYED"])
            .control("TP", [])
            .text([bold("Token Usage")])
            .text([roman("Input/output token counts and API costs")])
            .control("TP", [])
            .text([bold("Response Times")])
            .text([roman("API response latency and processing times")])
            .control("TP", [])
            .text([bold("Tool Execution")])
            .text([roman("Tool call statistics and execution times")])
            .control("TP", [])
            .text([bold("Memory Usage")])
            .text([roman("Memory consumption patterns")])
            .control("TP", [])
            .text([bold("Agent Performance")])
            .text([roman("Single-agent execution metrics")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Show performance metrics:")])
            .text([bold("  vtcode performance")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("vtcode-benchmark(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for the benchmark command
    fn generate_benchmark_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-BENCHMARK", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-benchmark - Run SWE-bench evaluation framework")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("benchmark"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Run automated performance testing against the SWE-bench evaluation framework."),
                roman(" Provides comparative analysis across different models, benchmark scoring,"),
                roman(" and optimization insights for coding tasks."),
            ])
            .control("SH", ["FEATURES"])
            .control("TP", [])
            .text([bold("Automated Testing")])
            .text([roman("Run standardized coding tasks and challenges")])
            .control("TP", [])
            .text([bold("Comparative Analysis")])
            .text([roman("Compare performance across different models")])
            .control("TP", [])
            .text([bold("Benchmark Scoring")])
            .text([roman("Quantitative performance metrics and scores")])
            .control("TP", [])
            .text([bold("Optimization Insights")])
            .text([roman("Recommendations for performance improvements")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Run benchmark suite:")])
            .text([bold("  vtcode benchmark")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("vtcode-performance(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for the create-project command
    fn generate_create_project_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-CREATE-PROJECT", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman(
                "vtcode-create-project - Create complete Rust project with features",
            )])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("create-project"),
                roman(" "),
                italic("NAME"),
                roman(" "),
                italic("FEATURES"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Create a complete Rust project with advanced features and integrations."),
                roman(" Supports web frameworks, database integration, authentication systems,"),
                roman(" testing setup, and security policies."),
            ])
            .control("SH", ["AVAILABLE FEATURES"])
            .text([roman("• web - Web framework (Axum, Rocket, Warp)")])
            .text([roman("• auth - Authentication system")])
            .text([roman("• db - Database integration")])
            .text([roman("• test - Testing setup")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Create web app with auth and database:")])
            .text([bold("  vtcode create-project myapp web,auth,db")])
            .text([roman("Create basic project:")])
            .text([bold("  vtcode create-project simple_app")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("vtcode-init(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for the init command
    fn generate_init_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-INIT", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-init - Guided AGENTS.md and workspace setup")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("init"),
                roman(" ["),
                bold("--force"),
                roman("]"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Bootstrap vtcode.toml, repository memory scaffolding, indexing,"),
                roman(" and a guided root AGENTS.md generated from repository signals."),
                roman(" Existing AGENTS.md files prompt for confirmation unless --force is used."),
            ])
            .control("SH", ["EXAMPLES"])
            .text([roman("Initialize current directory:")])
            .text([bold("  vtcode init")])
            .text([roman("Overwrite an existing AGENTS.md without prompting:")])
            .text([bold("  vtcode init --force")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("vtcode-create-project(1)")])
            .render();

        Ok(page)
    }

    /// Generate man page for the check command
    fn generate_check_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-CHECK", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-check - Run built-in repository checks")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("check"),
                roman(" "),
                bold("ast-grep"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Run built-in checks against the current workspace. The "),
                bold("ast-grep"),
                roman(" subcommand runs "),
                bold("ast-grep test --config sgconfig.yml"),
                roman(" followed by "),
                bold("ast-grep scan --config sgconfig.yml"),
                roman("."),
            ])
            .control("SH", ["PREREQUISITES"])
            .text([roman("Install ast-grep with:")])
            .text([bold("  vtcode dependencies install ast-grep")])
            .text([roman("Materialize the local scaffold with:")])
            .text([bold("  vtcode init")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Run ast-grep rule tests and scan:")])
            .text([bold("  vtcode check ast-grep")])
            .control("SH", ["SEE ALSO"])
            .text([
                bold("vtcode(1)"),
                roman(", "),
                bold("vtcode-init(1)"),
                roman(", "),
                bold("vtcode-man(1)"),
            ])
            .render();

        Ok(page)
    }

    /// Generate man page for the man command itself
    fn generate_man_man_page() -> Result<String> {
        let current_date = Self::current_date();
        let page = Roff::new()
            .control("TH", ["VTCODE-MAN", "1", &current_date, "VT Code", "User Commands"])
            .control("SH", ["NAME"])
            .text([roman("vtcode-man - Generate or display man pages for VT Code commands")])
            .control("SH", ["SYNOPSIS"])
            .text([
                bold("vtcode"),
                roman(" ["),
                bold("OPTIONS"),
                roman("] "),
                bold("man"),
                roman(" ["),
                italic("COMMAND"),
                roman("] ["),
                bold("--output"),
                roman(" "),
                italic("FILE"),
                roman("]"),
            ])
            .control("SH", ["DESCRIPTION"])
            .text([
                roman("Generate or display Unix man pages for VT Code commands. Man pages provide"),
                roman(" detailed documentation for all VT Code functionality including usage examples,"),
                roman(" option descriptions, and feature explanations."),
            ])
            .control("SH", ["OPTIONS"])
            .control("TP", [])
            .text([bold("--output"), roman(" "), italic("FILE")])
            .text([roman("Write man page to specified file instead of displaying")])
            .control("SH", ["AVAILABLE COMMANDS"])
            .text([roman("• chat - Interactive AI coding assistant")])
            .text([roman("• ask - Single prompt mode")])
            .text([roman("• analyse - Workspace analysis")])
            .text([roman("• performance - Performance metrics")])
            .text([roman("• trajectory - Pretty-print trajectory logs and analytics")])
            .text([roman("• benchmark - SWE-bench evaluation framework")])
            .text([roman("• create-project - Create complete Rust project with features")])
            .text([roman("• revert - Revert agent to a previous snapshot")])
            .text([roman("• snapshots - List available snapshots")])
            .text([roman("• cleanup-snapshots - Clean up old snapshots")])
            .text([roman("• init - Initialize project with enhanced structure")])
            .text([roman("• init-project - Initialize project with dot-folder structure")])
            .text([roman("• config - Generate configuration file")])
            .text([roman("• tool-policy - Manage tool execution policies")])
            .text([roman("• mcp - Manage Model Context Protocol providers")])
            .text([roman("• models - Manage models and providers")])
            .text([roman("• acp - Agent Client Protocol bridge for IDE integrations")])
            .text([roman("• chat-verbose - Verbose interactive chat with transparency")])
            .text([roman("• man - Man page generation (this command)")])
            .control("SH", ["EXAMPLES"])
            .text([roman("Display main VT Code man page:")])
            .text([bold("  vtcode man")])
            .text([roman("Display chat command man page:")])
            .text([bold("  vtcode man chat")])
            .text([roman("Save man page to file:")])
            .text([bold("  vtcode man chat --output chat.1")])
            .control("SH", ["SEE ALSO"])
            .text([bold("vtcode(1)"), roman(", "), bold("man(1)")])
            .render();

        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    //! Regression coverage for command-specific man-page dispatch.

    use super::ManPageGenerator;

    #[test]
    fn dispatches_canonical_and_legacy_analyse_man_pages() {
        let canonical_page =
            ManPageGenerator::generate_command_man_page("analyse").expect("analyse man page should be generated");
        let legacy_page = ManPageGenerator::generate_command_man_page("analyze")
            .expect("legacy analyze man page should dispatch to the canonical page");

        assert_eq!(legacy_page, canonical_page, "legacy analyze dispatch should return the canonical analyse man page");
        assert!(
            canonical_page.contains("VTCODE-ANALYSE"),
            "analyse dispatch should generate the canonical man-page title"
        );
        assert!(
            canonical_page.contains(r"\fBanalyse\fR"),
            "analyse man page should include the canonical command in its synopsis"
        );
    }
}
