//! CLI command dispatch surface.
//!
//! Splits command routing by concern: `commands` selects a subcommand and
//! runs it through the configured ACP adapters, `run` handles the interactive
//! chat, single-prompt, and session-resume entry points, and `skills` forwards
//! skill-management subcommands.
//!
//! The child modules own their implementations; this module re-exports the
//! entry points consumed by the top-level CLI parser so callers only depend on
//! this single dispatch boundary.

mod commands;
mod run;
mod skills;

pub(crate) use commands::dispatch_command_with_acp_adapters;
pub(crate) use run::{handle_ask_single_command, handle_chat_command, handle_resume_session_command};
