#![allow(
    clippy::blocks_in_conditions,
    clippy::filter_next,
    clippy::uninlined_format_args,
    clippy::expect_used,
    clippy::unwrap_used,
    missing_docs,
    reason = "The binary preserves existing compatibility lints while the workspace migration addresses actionable lint families."
)]
//! # Diapason - Terminal Coding Agent
//!
//! Diapason is a Rust-based terminal coding agent that pairs a streamlined
//! crossterm-powered interface with LLM-native code understanding and robust shell safety.
//! It is designed for developers who need precise context handling,
//! secure tool execution, and configurable multi-provider AI workflows.
//!
//! ## Highlights
//!
//! - **Multi-provider agent**: integrations for OpenAI, Anthropic,
//!   DeepSeek, Meta AI, Gemini, OpenRouter, and Ollama (local) with automatic failover and spend guards.
//! - **Semantic code understanding**: LLM-native code analysis and navigation (Rust, Python,
//!   JavaScript, TypeScript, Go, Java, and more) without the need for local grammar libraries.
//! - **Bash shell safety**: tree-sitter-bash integration for critical command validation
//!   and security enforcement.
//! - **Modern terminal experience**: inline renderer with streaming PTY output,
//!   slash commands, and customizable Ciapre-inspired theming.
//! - **Workspace-aware automation**: git-aware fuzzy navigation, workspace
//!   boundary enforcement, command allowlists, and human-in-the-loop
//!   confirmation.
//! - **Config-driven behaviour**: every agent control lives in `config.toml`,
//!   anchored by constants in `diapason_core::config::constants` and curated model
//!   metadata in `docs/models.json`.
//!
//! ## Quickstart
//!
//! ```bash
//! # Install the CLI (cargo, npm, or Homebrew are also supported)
//! cargo install diapason
//!
//! # Export the API key for your provider
//! export OPENAI_API_KEY="your-key"
//!
//! # Launch the agent with explicit provider/model overrides
//! diapason --provider openai --model gpt-5-codex
//!
//! # Run a one-off prompt with streaming output
//! diapason ask "Summarize diagnostics in src/lib.rs"
//!
//! # Perform a dry run without tool execution
//! diapason --no-tools ask "Review recent changes in src/main.rs"
//! ```
//!
//! Persist long-lived defaults in `config.toml` instead of hardcoding them:
//!
//! ```toml
//! [agent]
//! provider = "openai"
//! default_model = "gpt-5.6-sol"
//! ```
//!
//! The configuration loader resolves aliases through
//! `diapason_core::config::constants`, while `docs/models.json` tracks the latest
//! vetted provider model identifiers.
//!
//! ## Architecture Overview
//!
//! Diapason separates reusable library components from the CLI entrypoint:
//!
//! - `diapason-core/` exposes the agent runtime, provider abstractions (`llm/`),
//!   tool registry (`tools/`), configuration loaders, and shell safety
//!   integrations orchestrated with Tokio.
//! - `src/main.rs` embeds the inline UI, Clap-based CLI, and runtime wiring.
//! - MCP (Model Context Protocol) tools provide contextual resources (e.g.
//!   Serena MCP for journaling and memory), with policies expressed entirely in
//!   configuration.
//! - Safety features include workspace boundary enforcement, rate limiting,
//!   telemetry controls, and confirm-to-run guardrails.
//!
//! Additional implementation details live in `docs/ARCHITECTURE.md` and the
//! guides under `docs/project/`.
//!
//! ## Distribution Channels
//!
//! Diapason is distributed via multiple ecosystems:
//!
//! - **crates.io**: `cargo install diapason`
//! - **npm**: `npm install -g diapason`
//! - **Homebrew**: `brew install diapason`
//! - **GitHub Releases**: pre-built binaries for macOS, Linux, and Windows
//!
//! ## crates.io Categories
//!
//! This crate is listed under the following crates.io categories to improve
//! discoverability for terminal-focused developer tooling:
//!
//! - [`#development-tools`](https://crates.io/categories/development-tools)
//! - [`#command-line-utilities`](https://crates.io/categories/command-line-utilities)
//!
//! ## Library Usage Examples
//!
//! ### Starting an Agent Programmatically
//!
//! ```rust,ignore
//! use diapason_core::{Agent, DiapasonConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), anyhow::Error> {
//!     let config = DiapasonConfig::load()?;
//!     let agent = Agent::new(config).await?;
//!     agent.run().await?;
//!     Ok(())
//! }
//! ```
//!
//! ### Registering a Custom Tool
//!
//! ```rust,ignore
//! use diapason_core::tools::{ToolRegistry, ToolRegistration};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), anyhow::Error> {
//!     let workspace = std::env::current_dir()?;
//!     let mut registry = ToolRegistry::new(workspace);
//!
//!     let custom_tool = ToolRegistration {
//!         name: "my_custom_tool".into(),
//!         description: "A custom tool for specific tasks".into(),
//!         parameters: serde_json::json!({
//!             "type": "object",
//!             "properties": {
//!                 "input": {"type": "string"}
//!             }
//!         }),
//!         handler: |args| async move {
//!             // Tool implementation goes here
//!             Ok(serde_json::json!({"result": "success"}))
//!         },
//!     };
//!
//!     registry.register_tool(custom_tool).await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Agent Client Protocol (ACP)
//!
//! Diapason ships an ACP bridge tailored for Zed. Enable it via the `[acp]` section in
//! `config.toml`, launch `diapason acp`, and register the binary inside Zed's
//! `agent_servers` list. The full walkthrough lives in the
//! [Zed ACP integration guide](https://github.com/leynos/vtcode/tree/main/docs/guides/zed-acp.md),
//! and the rendered API guarantees appear on
//! [docs.rs](https://docs.rs/diapason/latest/diapason/#agent-client-protocol-acp).
//!
//! ### Bridge guarantees
//!
//! - Filesystem tooling stays disabled unless Zed advertises `fs.read_text_file`, so the agent
//!   never emits unsupported requests.
//! - Each `read_file` call is wrapped in `session/request_permission`, letting you approve or
//!   deny access to sensitive paths before any data leaves the editor.
//! - Cancellation signals from Zed halt streaming, mark pending tools as cancelled, and end the
//!   turn with `StopReason::Cancelled` for clean transcripts.
//! - ACP `plan` updates broadcast analysis, optional context gathering, and response drafting
//!   progress so Zed mirrors the agent's workflow.
//! - Strict absolute-path validation blocks relative or out-of-workspace arguments before they
//!   reach the client.
//! - When the model lacks tool calling, Diapason emits reasoning notices and downgrades to plain
//!   completions while keeping the plan timeline consistent.

//!
//! Diapason binary package
//!
//! This package contains the binary executable for Diapason.
//! For the core library functionality, see [`diapason-core`](https://docs.rs/diapason-core).

pub use diapason_acp as acp;

/// Re-exports from diapason-config's config watcher module.
pub mod config_watcher {
    pub use diapason_config::loader::watch::*;
}
pub use diapason_ui::tui::ui::interactive_list;
