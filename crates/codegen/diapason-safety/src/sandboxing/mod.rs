//! Sandboxing module for Diapason
//!
//! This module provides sandbox policies and execution environment transformations
//! inspired by the OpenAI Codex execution model and the AI sandbox field guide.
//! It enables safe command execution with configurable isolation levels.
//!
//! ## Architecture
//!
//! The sandboxing system implements the field guide's three-question model:
//! - **Boundary**: What is shared (kernel-enforced via Seatbelt/Landlock)
//! - **Policy**: What can code touch (SandboxPolicy enum)
//! - **Lifecycle**: What survives between runs (session-scoped approvals)
//!
//! Key components:
//! - **SandboxPolicy**: Configurable isolation levels (ReadOnly, WorkspaceWrite, DangerFullAccess)
//! - **SandboxManager**: Transforms command specifications into sandboxed execution environments
//! - **SandboxPermissions**: Fine-grained permission control for individual operations
//! - **NetworkAllowlistEntry**: Domain-based network egress control
//! - **SensitivePath**: Credential location blocking
//! - **ResourceLimits**: Memory, PID, disk, and CPU limits
//!
//! ## Usage
//!
//! ```rust,ignore
//! use diapason_core::sandboxing::{SandboxPolicy, SandboxManager, CommandSpec, ResourceLimits};
//!
//! let policy = SandboxPolicy::read_only();
//! let manager = SandboxManager::new();
//! let spec = CommandSpec {
//!     program: "cat".to_string(),
//!     args: vec!["file.txt".to_string()],
//!     ..Default::default()
//! };
//!
//! // Transform to sandboxed environment
//! let exec_env = manager.transform(spec, &policy, std::path::Path::new("/tmp"), None)?;
//! # Ok::<(), anyhow::Error>(())
//! ```

mod child_spawn;
mod debug;
mod exec_env;
mod manager;
mod permissions;
mod policy;

pub use child_spawn::{
    DIAPASON_SANDBOX_ACTIVE, DIAPASON_SANDBOX_NETWORK_DISABLED, DIAPASON_SANDBOX_TYPE, DIAPASON_SANDBOX_WRITABLE_ROOTS,
    FILTERED_ENV_VARS, PRESERVED_ENV_VARS, build_sanitized_env, filter_sensitive_env, setup_parent_death_signal,
    should_filter_env_var,
};
pub use debug::{
    DebugSubcommand, SandboxDebugResult, debug_sandbox, sandbox_capabilities_summary, test_network_blocked,
    test_path_writable,
};
pub use exec_env::{CommandSpec, ExecEnv, ExecExpiration, SandboxType};
pub use manager::{SandboxManager, SandboxTransformError};
pub use permissions::{AdditionalPermissions, SandboxPermissions};
pub use policy::{
    BLOCKED_SYSCALLS, DEFAULT_SENSITIVE_PATHS, FILTERED_SYSCALLS, NetworkAllowlistEntry, ResourceLimits, SandboxPolicy,
    SeccompProfile, SensitivePath, WritableRoot, default_sensitive_paths,
};
