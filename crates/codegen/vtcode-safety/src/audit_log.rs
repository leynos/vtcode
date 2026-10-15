//! Persistent audit logging for tool invocations.
//!
//! §18.4.4 of *The Hitchhiker's Guide to Agentic AI* calls for an audit log of
//! every tool call (arguments, outputs, timestamp). The existing
//! [`crate::command_safety::audit::SafetyAuditLogger`] only records command-safety
//! decisions in memory; this module adds a complementary, opt-in sink that
//! records every MCP and built-in tool invocation in a durable, append-only
//! JSONL stream.
//!
//! ## Sinks
//!
//! - [`JsonlFileSink`] — appends entries to a JSONL file with size-based rotation.
//! - [`InMemorySink`] — keeps entries in memory for tests and short-lived tooling.
//! - [`NullSink`] — discards everything; zero-cost when audit is disabled.
//! - [`MultiSink`] — fan-out wrapper used by [`ToolAuditLogger`].
//!
//! ## Threading
//!
//! Sinks are `Send + Sync`. Writes are non-blocking from the caller's perspective:
//! each sink uses an internal lock or background task to serialize I/O.
//!
//! See also `crates/codegen/vtcode-core/src/tools/untrusted_data.rs` for the prompt-injection
//! defense that pairs with this log.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod diagnostics;
mod sinks;
#[cfg(test)]
mod tests;

pub use sinks::{InMemorySink, JsonlFileSink, MultiSink, NullSink};

/// Status of a single tool invocation for audit purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAuditStatus {
    /// Tool ran to completion and produced a result.
    Success,
    /// Tool returned an error before completing.
    Failure,
    /// Tool exceeded its wall-clock budget.
    Timeout,
    /// Tool execution was cancelled (HITL refusal, planning denial, etc.).
    Cancelled,
    /// Tool execution was blocked before it started (e.g. loop detector, fuse).
    Blocked,
}

impl ToolAuditStatus {
    /// Short string label suitable for log lines.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
        }
    }
}

/// One audit row written per tool invocation.
///
/// The shape mirrors what `tracing` already records for MCP tool calls
/// (`mcp.tools.call` span with provider / transport / server metadata) but adds
/// enough context for offline forensics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAuditEntry {
    /// Unix epoch milliseconds when the entry was created.
    timestamp_unix_ms: u64,
    /// Stable identifier for the session (e.g. UUID).
    session_id: String,
    /// Stable identifier for the turn within the session.
    turn_id: String,
    /// Provider-side identifier for the tool call (matches `tool_call_id`).
    tool_call_id: String,
    /// Canonical tool name (e.g. `mcp::fetch::fetch`, `read_file`).
    tool_name: String,
    /// SHA-256 of the original (unredacted) tool arguments, hex-encoded.
    arguments_hash: String,
    /// Optional redacted snapshot of the arguments (secrets removed).
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments_redacted: Option<Value>,
    /// SHA-256 of the tool result, hex-encoded.
    result_hash: String,
    /// Optional first N characters of the result for offline triage.
    #[serde(skip_serializing_if = "Option::is_none")]
    result_summary: Option<String>,
    /// Wall-clock duration of the tool call in milliseconds.
    duration_ms: u64,
    /// Final status (success / failure / timeout / cancelled / blocked).
    status: ToolAuditStatus,
    /// Optional sandbox policy applied to this tool call (path to a JSON
    /// snapshot, or a short identifier).
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox_policy: Option<String>,
    /// MCP transport when applicable (`stdio`, `streamable_http`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    /// Remote server address when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    server_address: Option<String>,
    /// Remote server port when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    server_port: Option<u16>,
    /// Identifier of the model that requested the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    model_id: Option<String>,
    /// True when the static `is_suspicious_instruction` probe flagged the
    /// result content.
    prompt_injection_flagged: bool,
    /// Optional free-form reason (e.g. `loop_detector`, `circuit_breaker_open`).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Metadata and bodies supplied to [`ToolAuditEntry::from_invocation`]. The
/// body slices are hashed and never copied into the resulting entry.
pub struct ToolAuditInvocation<'a> {
    pub timestamp_unix_ms: u64,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub tool_call_id: &'a str,
    pub tool_name: &'a str,
    pub arguments: &'a [u8],
    pub result: &'a [u8],
    pub duration_ms: u64,
    pub status: ToolAuditStatus,
    pub model_id: Option<&'a str>,
    pub reason: Option<&'a str>,
}

/// Trait every audit sink implements.
pub trait ToolAuditSink: Send + Sync {
    /// Persist a single entry. Implementations must never block on network I/O
    /// (disk I/O is fine — JSONL writes are O(milliseconds) and buffered).
    fn write(&self, entry: &ToolAuditEntry) -> std::io::Result<()>;

    /// Flush any buffered entries to durable storage. Default no-op.
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Top-level audit logger — currently a thin wrapper that owns a single sink.
///
/// Future iterations can grow this into a fan-out by default (e.g. always
/// pair [`InMemorySink`] with the user's configured persistent sink).
#[derive(Clone)]
pub struct ToolAuditLogger {
    sink: Arc<dyn ToolAuditSink>,
}

impl std::fmt::Debug for ToolAuditLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolAuditLogger").finish_non_exhaustive()
    }
}

impl ToolAuditLogger {
    /// Construct a durable JSONL-backed audit logger.
    pub fn jsonl_file(path: impl Into<PathBuf>, max_size_bytes: u64, max_files: usize) -> std::io::Result<Self> {
        Ok(Self::new(Arc::new(JsonlFileSink::open(path, max_size_bytes, max_files)?)))
    }

    /// Wrap a sink in a logger.
    #[must_use]
    fn new(sink: Arc<dyn ToolAuditSink>) -> Self {
        Self { sink }
    }

    /// Disabled logger that drops every entry (equivalent to `NullSink`).
    #[must_use]
    fn disabled() -> Self {
        Self::new(Arc::new(NullSink))
    }

    /// Persist a single entry through the underlying sink.
    pub fn record(&self, entry: ToolAuditEntry) -> std::io::Result<()> {
        self.sink.write(&entry)
    }

    /// Flush the underlying sink.
    pub fn flush(&self) -> std::io::Result<()> {
        self.sink.flush()
    }

    /// Borrow the inner sink (used by tests and the CLI debug commands).
    #[must_use]
    pub fn sink(&self) -> &Arc<dyn ToolAuditSink> {
        &self.sink
    }
}

impl ToolAuditEntry {
    /// Build an entry from invocation metadata without retaining either body.
    ///
    /// The arguments and result are hashed immediately and are never included
    /// in the serialised entry. Callers should perform the surrounding file
    /// I/O on a blocking boundary.
    #[must_use]
    pub fn from_invocation(invocation: ToolAuditInvocation<'_>) -> Self {
        Self {
            timestamp_unix_ms: invocation.timestamp_unix_ms,
            session_id: invocation.session_id.to_owned(),
            turn_id: invocation.turn_id.to_owned(),
            tool_call_id: invocation.tool_call_id.to_owned(),
            tool_name: invocation.tool_name.to_owned(),
            arguments_hash: sha256_hex(invocation.arguments),
            arguments_redacted: None,
            result_hash: sha256_hex(invocation.result),
            result_summary: None,
            duration_ms: invocation.duration_ms,
            status: invocation.status,
            sandbox_policy: None,
            transport: None,
            server_address: None,
            server_port: None,
            model_id: invocation.model_id.map(str::to_owned),
            prompt_injection_flagged: false,
            reason: invocation.reason.map(str::to_owned),
        }
    }
}

impl Default for ToolAuditLogger {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Helper: SHA-256 hash of arbitrary bytes, hex-encoded.
///
/// Exposed so callers building a `ToolAuditEntry` can compute `arguments_hash`
/// / `result_hash` without pulling in `sha2` directly.
#[must_use]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ignored = write!(&mut out, "{byte:02x}");
    }
    out
}
