#![allow(
    missing_docs,
    clippy::expect_used,
    dead_code,
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
//! Command safety detection, execution policies, and sandboxing for Diapason.
//!
//! This crate provides the safety subsystem extracted from `diapason-core`:
//!
//! - **command_safety**: Granular command safety evaluation based on subcommands and options
//! - **exec_policy**: Execution authorization policies and approval requirements
//! - **sandboxing**: Sandbox policies and execution environment transformations

pub mod audit_log;
pub mod command_safety;
pub mod exec_policy;
pub mod mcp_sandbox;
pub mod sandboxing;
