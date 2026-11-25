//! Tool execution history and records.
//!
//! This module provides thread-safe recording and querying of tool executions,
//! including loop detection and rate limiting.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::config::constants::{defaults, tools};
use crate::tools::continuation::read_chunk_progress_from_result;
use crate::tools::tool_intent;

use super::execution_kernel::PATH_ALIAS_KEYS;

const READ_OFFSET_KEYS: &[&str] = &[
    "offset",
    "offset_lines",
    "offset_bytes",
    "byte_offset",
    "line_offset",
    "o",
    "line_start",
    "start_line",
];
const READ_LIMIT_KEYS: &[&str] = &[
    "limit",
    "limit_lines",
    "max_bytes",
    "page_size",
    "page_size_bytes",
    "byte_page_size",
    "page_size_lines",
    "line_page_size",
    "max_lines",
    "chunk_lines",
    "line_end",
    "end_line",
    "length",
];
const READ_EXTENT_KEYS: &[&str] = &[
    "offset",
    "offset_lines",
    "offset_bytes",
    "byte_offset",
    "line_offset",
    "o",
    "line_start",
    "start_line",
    "limit",
    "limit_lines",
    "max_bytes",
    "page_size",
    "page_size_bytes",
    "byte_page_size",
    "page_size_lines",
    "line_page_size",
    "max_lines",
    "chunk_lines",
    "line_end",
    "end_line",
    "length",
    "page",
    "per_page",
];

/// Result of loop detection analysis.
#[derive(Debug, Clone)]
pub struct LoopDetectionResult {
    /// Whether a loop was detected.
    pub detected: bool,
    /// Number of identical consecutive calls found.
    pub repeat_count: usize,
    /// Name of the tool being checked.
    pub tool_name: String,
}

/// Snapshot of harness context for execution records.
#[derive(Debug, Clone)]
pub struct HarnessContextSnapshot {
    pub session_id: String,
    pub task_id: Option<String>,
}

impl HarnessContextSnapshot {
    /// Create a new harness context snapshot.
    pub fn new(session_id: String, task_id: Option<String>) -> Self {
        Self { session_id, task_id }
    }

    /// Serialize snapshot for middleware/telemetry consumers without cloning callers.
    pub fn to_json(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "task_id": self.task_id,
        })
    }
}

/// Record of a single tool execution for diagnostics.
#[derive(Debug, Clone)]
pub struct ToolExecutionRecord {
    pub tool_name: String,
    pub requested_name: String,
    pub is_mcp: bool,
    pub mcp_provider: Option<String>,
    pub args: Value,
    pub result: Result<Value, String>,
    pub timestamp: SystemTime,
    pub success: bool,
    pub context: HarnessContextSnapshot,
    pub timeout_category: Option<String>,
    pub base_timeout_ms: Option<u64>,
    pub adaptive_timeout_ms: Option<u64>,
    pub effective_timeout_ms: Option<u64>,
    pub circuit_breaker: bool,
    pub attempt: u32,
    pub retry_after_ms: Option<u64>,
    pub circuit_breaker_state: Option<String>,
}

/// Aggregated tool-use telemetry for one repository task.
///
/// The public label maps internal legacy implementation names onto the current
/// model-facing tool surface so exported metrics do not reintroduce removed
/// tool names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTaskTelemetrySnapshot {
    pub task_id: Option<String>,
    pub total_tool_calls: usize,
    pub repeated_equivalent_calls: usize,
    pub failed_tool_calls: usize,
    pub spooled_outputs: usize,
    pub fallback_calls: usize,
    pub read_after_spool_calls: usize,
    pub command_approval_prompts: usize,
    pub task_completed_successfully: Option<bool>,
    pub calls_by_tool: BTreeMap<String, usize>,
}

impl ToolTaskTelemetrySnapshot {
    fn empty(task_id: Option<String>, task_completed_successfully: Option<bool>) -> Self {
        Self {
            task_id,
            total_tool_calls: 0,
            repeated_equivalent_calls: 0,
            failed_tool_calls: 0,
            spooled_outputs: 0,
            fallback_calls: 0,
            read_after_spool_calls: 0,
            command_approval_prompts: 0,
            task_completed_successfully,
            calls_by_tool: BTreeMap::new(),
        }
    }

    /// Export the snapshot as stable JSON for eval reports and trace fixtures.
    pub fn to_json(&self) -> Value {
        json!({
            "task_id": self.task_id,
            "total_tool_calls": self.total_tool_calls,
            "repeated_equivalent_calls": self.repeated_equivalent_calls,
            "failed_tool_calls": self.failed_tool_calls,
            "spooled_outputs": self.spooled_outputs,
            "fallback_calls": self.fallback_calls,
            "read_after_spool_calls": self.read_after_spool_calls,
            "command_approval_prompts": self.command_approval_prompts,
            "task_completed_successfully": self.task_completed_successfully,
            "calls_by_tool": self.calls_by_tool,
        })
    }
}

impl ToolExecutionRecord {
    /// Create a new failed execution record.
    #[expect(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    #[cold]
    pub fn failure(
        tool_name: String,
        requested_name: String,
        is_mcp: bool,
        mcp_provider: Option<String>,
        args: Value,
        error_msg: String,
        context: HarnessContextSnapshot,
        timeout_category: Option<String>,
        base_timeout_ms: Option<u64>,
        adaptive_timeout_ms: Option<u64>,
        effective_timeout_ms: Option<u64>,
        circuit_breaker: bool,
    ) -> Self {
        Self {
            tool_name,
            requested_name,
            is_mcp,
            mcp_provider,
            args,
            result: Err(error_msg),
            timestamp: SystemTime::now(),
            success: false,
            context,
            timeout_category,
            base_timeout_ms,
            adaptive_timeout_ms,
            effective_timeout_ms,
            circuit_breaker,
            attempt: 1,
            retry_after_ms: None,
            circuit_breaker_state: None,
        }
    }

    /// Create a new successful execution record.
    #[expect(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    #[inline]
    pub fn success(
        tool_name: String,
        requested_name: String,
        is_mcp: bool,
        mcp_provider: Option<String>,
        args: Value,
        result: Value,
        context: HarnessContextSnapshot,
        timeout_category: Option<String>,
        base_timeout_ms: Option<u64>,
        adaptive_timeout_ms: Option<u64>,
        effective_timeout_ms: Option<u64>,
        circuit_breaker: bool,
    ) -> Self {
        Self {
            tool_name,
            requested_name,
            is_mcp,
            mcp_provider,
            args,
            result: Ok(result),
            timestamp: SystemTime::now(),
            success: true,
            context,
            timeout_category,
            base_timeout_ms,
            adaptive_timeout_ms,
            effective_timeout_ms,
            circuit_breaker,
            attempt: 1,
            retry_after_ms: None,
            circuit_breaker_state: None,
        }
    }

    #[inline]
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt.max(1);
        self
    }

    #[inline]
    pub fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after_ms = retry_after.map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64);
        self
    }

    #[inline]
    pub fn with_circuit_breaker_state(mut self, state: impl Into<String>) -> Self {
        self.circuit_breaker_state = Some(state.into());
        self
    }
}

/// Default window size for loop detection.
///
/// A larger window gives the detector more context across turns, reducing
/// false positives when the model retries a call after a transient failure.
const DEFAULT_LOOP_DETECT_WINDOW: usize = 8;
/// Minimum limit for identical readonly operations.
///
/// Read/search calls are cheap to reuse but can become stale across unrelated
/// turns. The threshold must be high enough to allow one legitimate retry
/// (e.g. after an ast-grep parse error or a transient network failure) before
/// the loop detector fires.  A limit of 2 means the same identical call must
/// appear 2 times in the detection window before it is flagged.
const MIN_READONLY_IDENTICAL_LIMIT: usize = 2;
/// Maximum limit for identical readonly operations.
///
/// Mirrors the hard-block threshold in `execution_facade.rs`. Keeping this
/// in sync ensures `set_loop_detection_limits` can never raise the read-only
/// limit so high that every identical call immediately hard-blocks.
const MAX_READONLY_IDENTICAL_LIMIT: usize = 4;

fn spool_path_exists(result: &Value, workspace_root: &Path) -> bool {
    let Some(spool_path) = result.get("spool_path").and_then(|v| v.as_str()) else {
        return true;
    };

    let path = Path::new(spool_path);
    if path.is_absolute() {
        return path.exists();
    }

    workspace_root.join(path).exists()
        || path.exists()
        || env::current_dir().ok().is_some_and(|cwd| cwd.join(path).exists())
}

/// Check whether a spool path is still replayable. Mirrors `spool_path_exists`
/// but takes the path directly so it can be called from the unified TTL helper
/// without re-extracting the spool path from the result object.
fn spool_path_is_replayable(spool_path: &str, workspace_root: &Path) -> bool {
    let path = Path::new(spool_path);
    if path.is_absolute() {
        return path.exists();
    }

    workspace_root.join(path).exists()
        || path.exists()
        || env::current_dir().ok().is_some_and(|cwd| cwd.join(path).exists())
}

/// Whether a TTL replay requires the record to reference a spool file.
///
/// - `RequireSpool`: the caller only wants spool-backed payloads (PTY sessions,
///   large search outputs). Records without `spool_path` are skipped.
/// - `Any`: accept either an inline or spool-backed result, but always validate
///   that the spool file is still on disk when `spool_path` is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayMode {
    RequireSpool,
    Any,
}

fn read_file_path_from_args(args: &Value) -> Option<&str> {
    let obj = args.as_object()?;
    for key in PATH_ALIAS_KEYS {
        if let Some(path) = obj.get(key).and_then(|v| v.as_str()) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn normalize_tool_name_for_match(name: &str) -> String {
    let normalized = name.trim().to_ascii_lowercase().replace(' ', "_");
    tool_intent::canonical_command_session_tool_name(&normalized)
        .unwrap_or(&normalized)
        .to_string()
}

fn is_read_file_tool_name(name: &str) -> bool {
    let normalized = normalize_tool_name_for_match(name);
    normalized == tools::READ_FILE || normalized.ends_with(".read_file")
}

fn is_file_operation_tool_name(name: &str) -> bool {
    let normalized = normalize_tool_name_for_match(name);
    normalized == tools::UNIFIED_FILE || normalized.ends_with(".file_operation")
}

fn tool_name_matches(name: &str, expected: &str) -> bool {
    let normalized = normalize_tool_name_for_match(name);
    normalized == expected || normalized.ends_with(&format!(".{expected}"))
}

fn is_read_style_tool_call(tool_name: &str, args: &Value) -> bool {
    if tool_name_matches(tool_name, tools::READ_FILE) {
        return true;
    }
    if is_file_operation_tool_name(tool_name) {
        return tool_intent::file_operation_action_is(args, "read");
    }
    false
}

fn normalize_path_for_match(path: &str) -> String {
    path.trim().replace('\\', "/").trim_start_matches("./").to_string()
}

fn to_absolute_path(path: &str) -> Option<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw = Path::new(trimmed);
    if raw.is_absolute() {
        return Some(raw.to_path_buf());
    }
    env::current_dir().ok().map(|cwd| cwd.join(raw))
}

fn paths_match(record_path: &str, expected_path: &str) -> bool {
    let lhs = normalize_path_for_match(record_path);
    let rhs = normalize_path_for_match(expected_path);
    if lhs == rhs {
        return true;
    }
    if lhs.ends_with(&format!("/{rhs}")) || rhs.ends_with(&format!("/{lhs}")) {
        return true;
    }

    match (to_absolute_path(record_path), to_absolute_path(expected_path)) {
        (Some(abs_lhs), Some(abs_rhs)) => abs_lhs == abs_rhs,
        _ => false,
    }
}

fn is_read_file_style_record(record: &ToolExecutionRecord) -> bool {
    if is_read_file_tool_name(&record.tool_name) {
        return true;
    }

    if !is_file_operation_tool_name(&record.tool_name) {
        return false;
    }

    tool_intent::file_operation_action_is(&record.args, "read")
}

fn public_tool_telemetry_label(tool_name: &str) -> String {
    match tool_name {
        tools::UNIFIED_EXEC => tools::EXEC_COMMAND.to_string(),
        tools::UNIFIED_FILE => "file_operation".to_string(),
        _ => tool_name.to_string(),
    }
}

fn result_spool_path(record: &ToolExecutionRecord) -> Option<String> {
    record
        .result
        .as_ref()
        .ok()
        .and_then(|value| value.get("spool_path"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn arg_spool_path(record: &ToolExecutionRecord) -> Option<String> {
    record.args.get("spool_path").and_then(Value::as_str).map(str::to_string)
}

fn has_fallback_marker(record: &ToolExecutionRecord) -> bool {
    let Ok(result) = &record.result else {
        return false;
    };
    result.get("fallback_from").is_some()
        || result.get("fallback_to").is_some()
        || result.get("fallback_note").is_some()
}

fn command_requested_approval(record: &ToolExecutionRecord) -> bool {
    let label = public_tool_telemetry_label(&record.tool_name);
    if label != tools::EXEC_COMMAND {
        return false;
    }
    if let Ok(result) = &record.result
        && (result.get("approval_required").and_then(Value::as_bool).unwrap_or(false)
            || result.get("requires_approval").and_then(Value::as_bool).unwrap_or(false)
            || result
                .get("approval_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.trim().is_empty()))
    {
        return true;
    }
    let permissions = record
        .args
        .get("sandbox_permissions")
        .and_then(Value::as_str)
        .unwrap_or("use_default");
    matches!(permissions, "require_escalated" | "with_additional_permissions")
}

fn equivalent_call_key(record: &ToolExecutionRecord) -> String {
    let label = public_tool_telemetry_label(&record.tool_name);
    let args = serde_json::to_string(&record.args).unwrap_or_else(|_| "<non-json>".to_string());
    format!("{label}\0{args}")
}

/// Thread-safe execution history for recording tool executions.
#[derive(Clone)]
pub struct ToolExecutionHistory {
    records: Arc<RwLock<VecDeque<ToolExecutionRecord>>>,
    workspace_root: Arc<PathBuf>,
    max_records: usize,
    detect_window: Arc<std::sync::atomic::AtomicUsize>,
    identical_limit: Arc<std::sync::atomic::AtomicUsize>,
    rate_limit_per_minute: Arc<std::sync::atomic::AtomicUsize>,
}

impl ToolExecutionHistory {
    /// Create a new execution history with a maximum record count.
    pub fn new(max_records: usize) -> Self {
        Self::with_workspace_root(max_records, env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub(crate) fn with_workspace_root(max_records: usize, workspace_root: PathBuf) -> Self {
        Self {
            records: Arc::new(RwLock::new(VecDeque::with_capacity(max_records))),
            workspace_root: Arc::new(workspace_root),
            max_records,
            detect_window: Arc::new(std::sync::atomic::AtomicUsize::new(DEFAULT_LOOP_DETECT_WINDOW)),
            identical_limit: Arc::new(std::sync::atomic::AtomicUsize::new(defaults::DEFAULT_MAX_REPEATED_TOOL_CALLS)),
            rate_limit_per_minute: Arc::new(std::sync::atomic::AtomicUsize::new(
                crate::tools::rate_limit_config::tool_calls_per_minute_from_env().unwrap_or(0),
            )),
        }
    }

    /// Add a record to the history.
    pub fn add_record(&self, record: ToolExecutionRecord) {
        let Ok(mut records) = self.records.write() else {
            return;
        };
        records.push_back(record);
        while records.len() > self.max_records {
            records.pop_front();
        }
    }

    /// Set loop detection parameters.
    pub fn set_loop_detection_limits(&self, detect_window: usize, identical_limit: usize) {
        self.detect_window
            .store(detect_window.max(1), std::sync::atomic::Ordering::Relaxed);
        self.identical_limit
            .store(identical_limit, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the rate limit for tool executions per minute.
    pub fn set_rate_limit_per_minute(&self, limit: Option<usize>) {
        self.rate_limit_per_minute
            .store(limit.filter(|v| *v > 0).unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }

    /// Get the most recent records.
    pub fn get_recent_records(&self, count: usize) -> Vec<ToolExecutionRecord> {
        let Ok(records) = self.records.read() else {
            return Vec::new();
        };
        let records_len = records.len();
        let start = records_len.saturating_sub(count);
        records.iter().skip(start).cloned().collect()
    }

    /// Get recent failures in chronological order.
    pub fn get_recent_failures(&self, count: usize) -> Vec<ToolExecutionRecord> {
        let Ok(records) = self.records.read() else {
            return Vec::new();
        };
        let mut failures: Vec<ToolExecutionRecord> =
            records.iter().rev().filter(|r| !r.success).take(count).cloned().collect();
        failures.reverse();
        failures
    }

    /// Aggregate representative task telemetry from recorded tool calls.
    ///
    /// When `task_id` is `Some`, only records with the same harness task id are
    /// included. When `None`, the snapshot covers all stored records.
    pub fn task_telemetry_snapshot(
        &self,
        task_id: Option<&str>,
        task_completed_successfully: Option<bool>,
    ) -> ToolTaskTelemetrySnapshot {
        let snapshot_task_id = task_id.map(str::to_string);
        let mut snapshot = ToolTaskTelemetrySnapshot::empty(snapshot_task_id, task_completed_successfully);
        let Ok(records) = self.records.read() else {
            return snapshot;
        };

        let mut equivalent_calls_by_key: HashMap<String, usize> = HashMap::new();
        let mut seen_spool_paths: HashMap<String, usize> = HashMap::new();

        for record in records
            .iter()
            .filter(|record| task_id.is_none_or(|expected| record.context.task_id.as_deref() == Some(expected)))
        {
            snapshot.total_tool_calls += 1;
            let label = public_tool_telemetry_label(&record.tool_name);
            *snapshot.calls_by_tool.entry(label).or_default() += 1;

            if !record.success {
                snapshot.failed_tool_calls += 1;
            }
            let arg_spool_path = arg_spool_path(record);
            let result_spool_path = result_spool_path(record);
            if result_spool_path
                .as_deref()
                .is_some_and(|spool_path| arg_spool_path.as_deref() != Some(spool_path))
            {
                snapshot.spooled_outputs += 1;
            }
            if has_fallback_marker(record) {
                snapshot.fallback_calls += 1;
            }
            if command_requested_approval(record) {
                snapshot.command_approval_prompts += 1;
            }

            if let Some(spool_path) = arg_spool_path.as_ref()
                && seen_spool_paths.contains_key(spool_path)
            {
                snapshot.read_after_spool_calls += 1;
            }
            if let Some(spool_path) = result_spool_path
                && arg_spool_path.as_deref() != Some(spool_path.as_str())
            {
                *seen_spool_paths.entry(spool_path).or_default() += 1;
            }

            let count = equivalent_calls_by_key.entry(equivalent_call_key(record)).or_default();
            if *count > 0 {
                snapshot.repeated_equivalent_calls += 1;
            }
            *count += 1;
        }

        snapshot
    }

    /// Find the most recent spooled output for a tool call with identical args.
    pub fn find_recent_spooled_result(&self, tool_name: &str, args: &Value, max_age: Duration) -> Option<Value> {
        self.find_recent_matching(tool_name, args, max_age, ReplayMode::RequireSpool)
    }

    /// Find the most recent successful output for a tool call with identical args.
    pub fn find_recent_successful_result(&self, tool_name: &str, args: &Value, max_age: Duration) -> Option<Value> {
        self.find_recent_matching(tool_name, args, max_age, ReplayMode::Any)
    }

    /// Find the most recent successful output for a read-only tool call that
    /// targets the same file path and compatible read shape. This enables
    /// cross-turn dedup only when the cached read covers the new request.
    ///
    /// Returns `None` for non-read-only tools or when no matching path can be
    /// extracted from the args.
    pub fn find_recent_successful_by_read_target(
        &self,
        tool_name: &str,
        query_args: &Value,
        max_age: Duration,
    ) -> Option<Value> {
        let query_path = Self::extract_read_target(tool_name, query_args)?;
        self.find_recent_matching_with_predicate(tool_name, max_age, ReplayMode::Any, |record| {
            let record_path = Self::extract_read_target(tool_name, &record.args)?;
            if record_path != query_path {
                return None;
            }
            // Read-shape check: only match if the cached result covers the
            // query's extent and has the same raw/summarized mode.  A query
            // asking for a larger limit, different offset, or raw content is
            // a materially different read — the model genuinely needs fresh
            // content, not a cached stub.
            // Code-search targets already include the normalized effective
            // limit and filters. Comparing their raw arguments again would
            // reject an omitted default against the same explicit value.
            if tool_name != tools::CODE_SEARCH && !Self::read_extent_matches(&record.args, query_args) {
                return None;
            }
            Some(())
        })
    }

    /// Single source of truth for "find a recent successful record for this
    /// tool call, honouring the spool path lifetime semantics". Replaces the
    /// three near-identical loops that previously diverged on whether spool
    /// was required and how its existence was checked.
    fn find_recent_matching(
        &self,
        tool_name: &str,
        args: &Value,
        max_age: Duration,
        mode: ReplayMode,
    ) -> Option<Value> {
        self.find_recent_matching_with_predicate(tool_name, max_age, mode, |record| {
            (record.args == *args).then_some(())
        })
    }

    fn find_recent_matching_with_predicate(
        &self,
        tool_name: &str,
        max_age: Duration,
        mode: ReplayMode,
        mut matches: impl FnMut(&ToolExecutionRecord) -> Option<()>,
    ) -> Option<Value> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        let now = SystemTime::now();
        let mut later_mutated_paths = Vec::new();
        let mut later_pathless_mutation = false;

        for record in records.iter().rev() {
            if record.success && tool_intent::classify_tool_intent(&record.tool_name, &record.args).mutating {
                let mutation_paths = crate::tools::mutation_target_paths(&record.tool_name, &record.args);
                later_pathless_mutation |= mutation_paths.is_empty();
                later_mutated_paths.extend(mutation_paths);
            }
            if record.tool_name != tool_name || !record.success {
                continue;
            }
            if matches(record).is_none() {
                continue;
            }

            let age_ok = match now.duration_since(record.timestamp) {
                Ok(age) => age <= max_age,
                Err(_) => false,
            };
            if !age_ok {
                continue;
            }

            if record.tool_name == tools::CODE_SEARCH
                && (later_pathless_mutation
                    || later_mutated_paths.iter().any(|mutated_path| {
                        crate::tools::code_search::scope_contains_mutated_path(
                            &record.args,
                            mutated_path,
                            self.workspace_root.as_ref(),
                        )
                    }))
            {
                continue;
            }

            let Ok(result) = &record.result else {
                continue;
            };

            if let Some(spool_path) = result.get("spool_path").and_then(Value::as_str) {
                if mode == ReplayMode::RequireSpool && !spool_path_exists(result, &self.workspace_root) {
                    continue;
                }
                // Single source of truth for spool-path existence. Uses the
                // shared helper so a relative path resolves against the
                // workspace cwd consistently across all callers.
                if !spool_path_is_replayable(spool_path, &self.workspace_root) {
                    continue;
                }
            } else if mode == ReplayMode::RequireSpool {
                continue;
            }

            return Some(result.clone());
        }
        None
    }

    /// Invalidate cache records whose read target overlaps the mutated path.
    /// Used when a write tool modifies a file: only the records that could
    /// contain stale content for that file are dropped, instead of wiping
    /// every cached read-only result.
    pub fn invalidate_for_path(&self, target_path: &str) {
        let Ok(mut records) = self.records.write() else {
            return;
        };
        records.retain(|record| {
            if record.tool_name == tools::READ_FILE || record.tool_name == tools::UNIFIED_FILE {
                if let Some(record_path) = Self::extract_read_target(&record.tool_name, &record.args) {
                    if record_path == target_path {
                        return false;
                    }
                }
            }
            true
        });
    }

    /// Conservatively drop every cached read-only result.
    ///
    /// Used when a mutating shell command produces no identifiable target path
    /// (e.g. `sed -i`), so we cannot know which files may now be stale. A
    /// pathless mutation could have touched anything, so no read record can be
    /// trusted afterwards.
    pub fn invalidate_all_reads(&self) {
        let Ok(mut records) = self.records.write() else {
            return;
        };
        records.retain(|record| !(record.tool_name == tools::READ_FILE || record.tool_name == tools::UNIFIED_FILE));
    }

    /// Check whether the cached record's read shape covers the new query's shape.
    ///
    /// Non-range arguments must match exactly. Range aliases are normalized
    /// only after their values are validated, and the cached range must cover
    /// the query range. This prevents replaying a different slice, encoding,
    /// pagination page, or read mode (issue #680).
    fn read_extent_matches(cached_args: &Value, query_args: &Value) -> bool {
        let Some(cached_shape) = Self::read_shape_without_extent(cached_args) else {
            return false;
        };
        let Some(query_shape) = Self::read_shape_without_extent(query_args) else {
            return false;
        };
        if cached_shape != query_shape {
            return false;
        }

        let Ok(cached_offset) = read_extent_value(cached_args, READ_OFFSET_KEYS) else {
            return false;
        };
        let Ok(query_offset) = read_extent_value(query_args, READ_OFFSET_KEYS) else {
            return false;
        };
        if !compatible_extent_values(cached_offset, query_offset, true) {
            return false;
        }

        let Ok(cached_limit) = read_extent_value(cached_args, READ_LIMIT_KEYS) else {
            return false;
        };
        let Ok(query_limit) = read_extent_value(query_args, READ_LIMIT_KEYS) else {
            return false;
        };
        if !compatible_extent_values(cached_limit, query_limit, false) {
            return false;
        }

        let Ok(cached_page) = read_page_extent(cached_args) else {
            return false;
        };
        let Ok(query_page) = read_page_extent(query_args) else {
            return false;
        };
        cached_page == query_page
    }

    fn read_shape_without_extent(args: &Value) -> Option<Value> {
        let mut object = args.as_object()?.clone();
        for key in PATH_ALIAS_KEYS.iter().chain(READ_EXTENT_KEYS.iter()) {
            object.remove(*key);
        }
        Some(Value::Object(object))
    }

    /// Extract the read target from tool args for path-based matching.
    /// Returns `None` for non-read-only tools or when no path is found.
    ///
    /// For search tools, the key includes the normalized query identity so
    /// different searches on the same directory are not treated as duplicates.
    fn extract_read_target(tool_name: &str, args: &Value) -> Option<String> {
        let obj = args.as_object()?;
        let is_read = match tool_name {
            tools::READ_FILE | tools::GREP_FILE | tools::LIST_FILES | tools::CODE_SEARCH => true,
            tools::UNIFIED_FILE => {
                matches!(obj.get("action").and_then(Value::as_str), Some("read"))
            }
            _ => false,
        };
        if !is_read {
            return None;
        }
        if tool_name == tools::CODE_SEARCH {
            return crate::tools::normalized_code_search_identity(args);
        }
        let path = Self::extract_path_from_args(obj)?;
        if tool_name == tools::GREP_FILE {
            let pattern = obj.get("pattern").and_then(Value::as_str).unwrap_or("");
            return Some(format!("{path}::{pattern}"));
        }
        Some(path)
    }

    fn extract_path_from_args(obj: &serde_json::Map<String, Value>) -> Option<String> {
        for key in PATH_ALIAS_KEYS {
            if let Some(path) = obj.get(key).and_then(Value::as_str) {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
    ///
    /// Supports both `read_file` and `file_operation` read action records.
    ///
    /// Returns `(next_offset, chunk_limit)` when the recent call indicates more chunks are
    /// available (`spool_chunked=true`, `has_more=true`).
    pub fn find_recent_read_file_spool_progress(&self, path: &str, max_age: Duration) -> Option<(usize, usize)> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        let now = SystemTime::now();
        let expected_path = path.trim();

        for record in records.iter().rev() {
            if !record.success || !is_read_file_style_record(record) {
                continue;
            }

            let Some(record_path) = read_file_path_from_args(&record.args) else {
                continue;
            };
            if !paths_match(record_path, expected_path) {
                continue;
            }

            let age_ok = match now.duration_since(record.timestamp) {
                Ok(age) => age <= max_age,
                Err(_) => false,
            };
            if !age_ok {
                continue;
            }

            let Ok(result) = &record.result else {
                continue;
            };
            let chunked = result.get("spool_chunked").and_then(|v| v.as_bool()).unwrap_or(false);
            let has_more = result.get("has_more").and_then(|v| v.as_bool()).unwrap_or(false);
            if !(chunked && has_more) {
                continue;
            }

            if let Some(progress) = read_chunk_progress_from_result(result) {
                return Some(progress);
            }
        }
        None
    }

    /// Clear all records.
    pub fn clear(&self) {
        if let Ok(mut records) = self.records.write() {
            records.clear();
        }
    }

    /// Total number of execution records currently stored.
    pub fn len(&self) -> usize {
        self.records.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether no execution records are currently stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the current loop limit.
    pub fn loop_limit(&self) -> usize {
        self.identical_limit.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the effective loop limit for a specific tool.
    pub fn loop_limit_for(&self, tool_name: &str, args: &Value) -> usize {
        self.effective_identical_limit_for_call(tool_name, args)
    }

    /// Get the rate limit per minute if configured.
    pub fn rate_limit_per_minute(&self) -> Option<usize> {
        let val = self.rate_limit_per_minute.load(std::sync::atomic::Ordering::Relaxed);
        (val != 0).then_some(val)
    }

    fn effective_identical_limit_for_call(&self, tool_name: &str, args: &Value) -> usize {
        let base_limit = self.identical_limit.load(std::sync::atomic::Ordering::Relaxed);
        if is_read_style_tool_call(tool_name, args) || tool_name_matches(tool_name, tools::CODE_SEARCH) {
            // Read-only tools: clamp to [MIN, MAX] so the limit cannot grow
            // unbounded via set_loop_detection_limits, while still allowing
            // callers to lower it below the default for aggressive dedup.
            base_limit.clamp(MIN_READONLY_IDENTICAL_LIMIT, MAX_READONLY_IDENTICAL_LIMIT)
        } else {
            base_limit
        }
    }

    /// Count calls within a time window.
    pub fn calls_in_window(&self, window: Duration) -> usize {
        let cutoff = SystemTime::now().checked_sub(window).unwrap_or(SystemTime::UNIX_EPOCH);

        let Ok(records) = self.records.read() else {
            return 0;
        };
        records.iter().rev().take_while(|record| record.timestamp >= cutoff).count()
    }

    /// Detect if the agent is stuck in a loop.
    ///
    /// Returns a `LoopDetectionResult` indicating whether a loop was detected.
    pub fn detect_loop(&self, tool_name: &str, args: &Value) -> LoopDetectionResult {
        let limit = self.effective_identical_limit_for_call(tool_name, args);
        if limit == 0 {
            return LoopDetectionResult {
                detected: false,
                repeat_count: 0,
                tool_name: tool_name.to_string(),
            };
        }

        let detect_window = self.detect_window.load(std::sync::atomic::Ordering::Relaxed);
        let window = detect_window.max(limit.saturating_mul(2)).max(1);

        let Ok(records) = self.records.read() else {
            return LoopDetectionResult {
                detected: false,
                repeat_count: 0,
                tool_name: tool_name.to_string(),
            };
        };
        let recent: Vec<&ToolExecutionRecord> = records.iter().rev().take(window).collect();

        if recent.is_empty() {
            return LoopDetectionResult {
                detected: false,
                repeat_count: 0,
                tool_name: tool_name.to_string(),
            };
        }

        // Count how many recent calls match this tool's loop identity.
        // CRITICAL FIX: Only count SUCCESSFUL calls to avoid cascade blocking
        let mut identical_count = 0;
        for record in &recent {
            let same_args = if tool_name_matches(tool_name, tools::CODE_SEARCH) {
                crate::tools::normalized_code_search_loop_identity(&record.args)
                    == crate::tools::normalized_code_search_loop_identity(args)
            } else {
                record.args == *args
            };
            if record.tool_name == tool_name && same_args && record.success {
                identical_count += 1;
            }
        }

        let detected = identical_count >= limit;
        LoopDetectionResult {
            detected,
            repeat_count: identical_count,
            tool_name: tool_name.to_string(),
        }
    }
}

fn read_extent_value(args: &Value, keys: &[&'static str]) -> Result<Option<(&'static str, u64)>, ()> {
    let object = args.as_object().ok_or(())?;
    let mut found = None;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        let value = value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.trim().parse::<u64>().ok()))
            .ok_or(())?;
        if found.is_some() {
            return Err(());
        }
        found = Some((*key, value));
    }
    Ok(found)
}

fn compatible_extent_values(
    cached: Option<(&'static str, u64)>,
    query: Option<(&'static str, u64)>,
    default_zero_is_compatible: bool,
) -> bool {
    match (cached, query) {
        (Some((cached_key, cached_value)), Some((query_key, query_value))) => {
            cached_key == query_key
                && if default_zero_is_compatible {
                    cached_value == query_value
                } else {
                    cached_value >= query_value
                }
        }
        (None, None) => true,
        (Some((_, value)), None) if default_zero_is_compatible => value == 0,
        (None, Some((_, value))) if default_zero_is_compatible => value == 0,
        _ => false,
    }
}

fn read_page_extent(args: &Value) -> Result<(Option<u64>, Option<u64>), ()> {
    let page = read_extent_value(args, &["page"])?.map(|(_, value)| value);
    let per_page = read_extent_value(args, &["per_page"])?.map(|(_, value)| value);
    Ok((page, per_page))
}

impl Default for ToolExecutionHistory {
    fn default() -> Self {
        Self::new(100)
    }
}

/// Tests for execution-history replay, loop identity, and invalidation.
#[cfg(test)]
#[path = "execution_history_tests/mod.rs"]
mod tests;
