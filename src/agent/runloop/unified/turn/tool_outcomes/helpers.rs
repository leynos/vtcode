use std::path::{Path, PathBuf};
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use vtcode_core::llm::provider as uni;
use vtcode_core::tools::names::canonical_tool_name;
use vtcode_core::tools::tool_intent::{
    ShellActivity, classify_shell_activity, shell_command_is_admitted_verification_attempt,
};

use crate::agent::runloop::unified::tool_pipeline::{ToolExecutionStatus, ToolPipelineOutcome};
use crate::agent::runloop::unified::turn::tool_outcomes::read_extent;
use crate::agent::runloop::unified::turn::tool_outcomes::{is_grep_style_no_match, output_field_is_empty};

/// Threshold: number of consecutive file mutations before the Anti-Blind-Editing
/// warning fires. NL2Repo-Bench recommends verifying after every few edits.
pub(crate) const BLIND_EDITING_THRESHOLD: usize = 4;
pub(crate) const ANTI_BLIND_EDITING_WARNING: &str = "[!] Anti-Blind-Editing: Pause to run verification/tests.";
pub(crate) const ANTI_BLIND_EDITING_DIRECTIVE: &str = "CRITICAL: Multiple edits were made without verification. Stop editing and run `exec_command` to compile or test before proceeding.";
/// Fix-up window granted after a failed verification attempt. A failed
/// `cargo check` / `cargo nextest run` must not deadlock the turn: the agent
/// needs a bounded number of edits to address the reported failure before
/// re-verifying. Each failed verifier refreshes this window, so blind editing
/// (many edits with no verifier attempt) stays blocked while fix-verify loops
/// can make progress.
pub(crate) const FAILED_VERIFICATION_FIX_ALLOWANCE: u8 = 2;

/// Threshold: number of consecutive read/search operations before the Navigation
/// Loop warning fires.
pub(crate) const NAVIGATION_LOOP_THRESHOLD: usize = 15;

/// Planning recovery thresholds for low-signal navigation. These are kept
/// below the hard planning tool-call ceiling so the model gets one bounded,
/// tool-free synthesis pass while the evidence is still useful.
pub(crate) const PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD: u8 = 6;
pub(crate) const PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD: u8 = 10;

/// Optimized loop detection with bounded signature keys and exponential backoff.
pub(crate) struct LoopTracker {
    attempts: FxHashMap<String, (usize, Instant)>,
    low_signal_attempts: FxHashMap<String, (usize, Instant)>,
    /// Counter for consecutive mutating file operations without execution/verification
    pub consecutive_mutations: usize,
    /// True after the mutation threshold until a verification command completes.
    pub verification_pending: bool,
    /// Bounded fix-up edits allowed while verification stays pending.
    /// Set to [`FAILED_VERIFICATION_FIX_ALLOWANCE`] after a failed verifier so
    /// a broken build can be repaired; consumed by successful fix-up mutations.
    /// Persisted in `SessionStats` so `continue` turns keep the same window.
    pub fix_edits_remaining: u8,
    /// Prevent repeated warning output while verification remains pending.
    pub verification_warning_emitted: bool,
    /// Prevent repeated inline block notices for a single verification checkpoint.
    pub verification_block_notice_emitted: bool,
    /// Counter for consecutive read/search operations without action or synthesis
    pub consecutive_navigations: usize,
    /// Number of times navigation-loop recovery has fired in this session.
    pub navigation_loop_recoveries: usize,
    /// Consecutive low-signal navigation outcomes in this turn.
    pub consecutive_low_signal_navigations: u8,
    /// Total low-signal navigation outcomes in this turn.
    pub total_low_signal_navigations: u8,
    /// Lifetime low-signal outcomes for checkpoint diagnostics. Unlike the
    /// adaptive window counters, this never resets within the turn.
    pub low_signal_tool_calls: u32,
    /// At most one adaptive planning synthesis pass is scheduled per turn.
    pub planning_low_signal_synthesis_triggered: bool,
    /// Unique navigation signatures in the current consecutive window.
    /// Used to distinguish legitimate exploration (all unique) from actual looping (many repeats).
    nav_signatures: FxHashSet<String>,
}

impl LoopTracker {
    pub(crate) fn new() -> Self {
        Self {
            attempts: FxHashMap::with_capacity_and_hasher(16, Default::default()),
            low_signal_attempts: FxHashMap::with_capacity_and_hasher(8, Default::default()),
            consecutive_mutations: 0,
            verification_pending: false,
            fix_edits_remaining: 0,
            verification_warning_emitted: false,
            verification_block_notice_emitted: false,
            consecutive_navigations: 0,
            navigation_loop_recoveries: 0,
            consecutive_low_signal_navigations: 0,
            total_low_signal_navigations: 0,
            low_signal_tool_calls: 0,
            planning_low_signal_synthesis_triggered: false,
            nav_signatures: FxHashSet::default(),
        }
    }

    /// Tuple counterpart to `SessionStats::verification_snapshot`, so turn
    /// setup and persistence share one call shape instead of threading two
    /// loosely-coupled halves across five call sites. A zero-pending snapshot
    /// never carries fix-ups; the clamp keeps a stale caller from building an
    /// inconsistent gate.
    pub(crate) fn with_verification_snapshot(snapshot: (bool, u8)) -> Self {
        let mut tracker = Self::new();
        tracker.verification_pending = snapshot.0;
        tracker.fix_edits_remaining = if snapshot.0 { snapshot.1 } else { 0 };
        tracker
    }

    /// Record an attempt and return the count
    pub(crate) fn record(&mut self, signature: String) -> usize {
        let entry = self.attempts.entry(signature).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
        entry.0
    }

    fn record_low_signal(&mut self, signature: String) -> usize {
        let entry = self.low_signal_attempts.entry(signature).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
        entry.0
    }

    /// Get the maximum repetition count, optionally filtering by a predicate on the signature
    pub(crate) fn max_count_filtered<F>(&self, exclude: F) -> usize
    where
        F: Fn(&str) -> bool,
    {
        self.attempts
            .iter()
            .filter_map(|(sig, (count, _))| if exclude(sig) { None } else { Some(*count) })
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn max_low_signal_count(&self) -> usize {
        self.low_signal_attempts.values().map(|(count, _)| *count).max().unwrap_or(0)
    }

    /// Number of redundant navigations (total - unique) in the current window.
    /// At least 3 before the navigation loop guard considers firing.
    pub(crate) fn repeated_navigation_count(&self) -> usize {
        self.consecutive_navigations.saturating_sub(self.nav_signatures.len())
    }

    fn reset_low_signal_attempts(&mut self) {
        self.low_signal_attempts.clear();
    }

    fn reset_low_signal_navigation_counters(&mut self) {
        self.consecutive_low_signal_navigations = 0;
        self.total_low_signal_navigations = 0;
    }

    /// Clear the per-turn navigation window after a non-navigation tool.
    /// Callers pass `low_signal_family.is_none()` so diverse productive reads
    /// keep their repetition history while low-signal churn resets.
    fn reset_navigation_window(&mut self, clear_low_signal_attempts: bool) {
        self.consecutive_navigations = 0;
        self.nav_signatures.clear();
        self.reset_low_signal_navigation_counters();
        if clear_low_signal_attempts {
            self.reset_low_signal_attempts();
        }
    }

    fn record_navigation_signal(&mut self, is_low_signal: bool) {
        if is_low_signal {
            self.low_signal_tool_calls = self.low_signal_tool_calls.saturating_add(1);
            self.consecutive_low_signal_navigations = self.consecutive_low_signal_navigations.saturating_add(1);
            self.total_low_signal_navigations = self.total_low_signal_navigations.saturating_add(1);
        } else {
            // Productive inspection breaks only the consecutive streak. The
            // total remains turn-scoped so diverse empty searches still
            // converge on synthesis.
            self.consecutive_low_signal_navigations = 0;
        }
    }

    pub(crate) fn reset_after_balancer_recovery(&mut self) {
        self.attempts.clear();
        self.low_signal_attempts.clear();
        self.nav_signatures.clear();
        self.consecutive_mutations = 0;
        self.verification_block_notice_emitted = false;
        self.consecutive_navigations = 0;
        self.reset_low_signal_navigation_counters();
    }

    pub(crate) fn verification_is_pending(&self) -> bool {
        self.verification_pending || self.consecutive_mutations >= BLIND_EDITING_THRESHOLD
    }

    /// Snapshot the session-persisted gate state for `SessionStats`.
    /// Persist both halves together so resumed turns reconstruct the same
    /// gate instead of drifting (a pending gate with a lost fix window
    /// deadlocks a broken build).
    pub(crate) fn verification_snapshot(&self) -> (bool, u8) {
        (self.verification_is_pending(), self.fix_edits_remaining)
    }

    pub(crate) fn mark_verification_pending(&mut self) {
        self.verification_pending = true;
    }

    /// Grant a bounded fix-up window after a failed verifier. The gate stays
    /// pending (completion still requires a successful standalone verifier),
    /// but the next [`FAILED_VERIFICATION_FIX_ALLOWANCE`] successful mutations
    /// are admitted so a broken build can be repaired instead of deadlocking.
    pub(crate) fn record_failed_verification(&mut self) {
        self.verification_pending = true;
        self.fix_edits_remaining = FAILED_VERIFICATION_FIX_ALLOWANCE;
    }

    fn record_successful_mutation(&mut self) {
        // Consume the fix-up window first: repair edits must not grow the
        // blind-editing counter while the gate already requires re-verify.
        if self.verification_pending && self.fix_edits_remaining > 0 {
            self.fix_edits_remaining = self.fix_edits_remaining.saturating_sub(1);
            return;
        }
        self.consecutive_mutations = self.consecutive_mutations.saturating_add(1);
        if self.consecutive_mutations >= BLIND_EDITING_THRESHOLD {
            self.verification_pending = true;
        }
    }

    fn mark_verification_complete(&mut self) {
        self.consecutive_mutations = 0;
        self.verification_pending = false;
        self.fix_edits_remaining = 0;
        self.verification_warning_emitted = false;
        self.verification_block_notice_emitted = false;
    }
}

/// Check if an identical tool call (same name + same args) was already executed
/// recently in the working history. Returns the output of the most recent
/// matching tool response if found.
///
/// This catches cross-turn duplicates that the per-turn `LoopTracker` misses
/// because it is reset at the start of each turn. Scans the last
/// `MAX_HISTORY_SCAN` messages to keep the check bounded.
///
/// File-read pagination is normalized so that re-reading the same file with a
/// different `offset` or `limit` is recognized as the same logical read.
/// `code_search` uses a separate replay identity that retains the effective
/// `max_results`; its loop identity is separate.
///
/// Tool-call IDs are scoped to the nearest preceding Assistant batch. A later
/// batch may reuse an ID for another tool, so both the batch and tool name must
/// match before its Tool response can satisfy this replay lookup.
pub(crate) fn find_duplicate_in_history(
    history: &[uni::Message],
    tool_name: &str,
    args: &serde_json::Value,
    workspace_root: &Path,
) -> Option<String> {
    const MAX_HISTORY_SCAN: usize = 120;
    let target_signature = read_normalized_signature_key(tool_name, args);

    let scan_start = history.len().saturating_sub(MAX_HISTORY_SCAN);
    let target_tool_name = canonical_tool_name(tool_name);
    let mut current_batch: FxHashMap<String, (String, serde_json::Value)> = FxHashMap::default();
    let mut matching_responses = Vec::new();

    for (offset, msg) in history[scan_start..].iter().enumerate() {
        let abs_idx = scan_start + offset;
        match msg.role {
            uni::MessageRole::Assistant => {
                current_batch.clear();
                if let Some(ref tool_calls) = msg.tool_calls {
                    for tc in tool_calls {
                        if let Some(ref func) = tc.function {
                            let tc_args: serde_json::Value = serde_json::from_str(&func.arguments)
                                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
                            current_batch.insert(tc.id.clone(), (canonical_tool_name(&func.name).to_string(), tc_args));
                        }
                    }
                }
            }
            uni::MessageRole::Tool => {
                let Some(call_id) = msg.tool_call_id.as_deref() else {
                    continue;
                };
                let Some((batch_tool_name, tc_args)) = current_batch.get(call_id) else {
                    continue;
                };
                if batch_tool_name == target_tool_name
                    && read_normalized_signature_key(batch_tool_name, tc_args) == target_signature
                    && read_extent::extent_covers(tc_args, args)
                    && tool_response_is_replayable(msg)
                {
                    matching_responses.push((abs_idx, tc_args.clone(), msg));
                }
            }
            _ => {}
        }
    }

    for (response_index, tc_args, msg) in matching_responses.into_iter().rev() {
        let invalidated = tool_name == vtcode_core::config::constants::tools::CODE_SEARCH
            && history_has_scoped_mutation_after(history, response_index, &tc_args, workspace_root);
        if !invalidated {
            return Some(msg.content.as_text().to_string());
        }
    }
    None
}

fn tool_response_is_replayable(message: &uni::Message) -> bool {
    let content = message.content.as_text();
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.len() > 128 * 1024 {
        return false;
    }

    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(output)) => {
            if output.contains_key("error") || output.contains_key("error_type") || output.contains_key("failure_kind")
            {
                return false;
            }
            if output.get("blocked").and_then(serde_json::Value::as_bool) == Some(true)
                || output.get("verification_required").and_then(serde_json::Value::as_bool) == Some(true)
            {
                return false;
            }
            if matches!(output.get("success"), Some(serde_json::Value::Bool(false)) | Some(serde_json::Value::Null)) {
                return false;
            }
            if output.get("success").is_some_and(|value| !value.is_boolean()) {
                return false;
            }
            !output.get("status").and_then(serde_json::Value::as_str).is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "failed"
                        | "failure"
                        | "error"
                        | "denied"
                        | "permission_denied"
                        | "rejected"
                        | "timeout"
                        | "timed_out"
                        | "cancelled"
                        | "canceled"
                        | "interrupted"
                        | "aborted"
                        | "blocked"
                        | "skipped"
                        | "not_started"
                        | "not_executed"
                        | "pending"
                        | "in_progress"
                        | "not_run"
                )
            })
        }
        Ok(serde_json::Value::String(value)) => text_response_is_replayable(&value),
        Ok(serde_json::Value::Array(_) | serde_json::Value::Number(_) | serde_json::Value::Bool(_)) => true,
        Ok(serde_json::Value::Null) => false,
        Err(_) => text_response_is_replayable(trimmed),
    }
}

fn text_response_is_replayable(content: &str) -> bool {
    let trimmed = content.trim();
    const FAILURE_PREFIXES: &[&str] = &[
        "error:",
        "execution denied",
        "permission denied",
        "timeout",
        "timed out",
        "cancelled",
        "canceled",
        "failed",
        "failure",
        "denied",
        "rejected",
        "blocked",
        "aborted",
        "interrupted",
        "skipped",
        "not started",
        "not executed",
        "not run",
        "pending",
        "in progress",
    ];
    !FAILURE_PREFIXES.iter().any(|prefix| {
        trimmed
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
    })
}

fn history_has_scoped_mutation_after(
    history: &[uni::Message],
    response_index: usize,
    search_args: &serde_json::Value,
    workspace_root: &Path,
) -> bool {
    let mut pending_mutations: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
    for message in history.iter().skip(response_index.saturating_add(1)) {
        match message.role {
            uni::MessageRole::Assistant => {
                // Tool-call IDs are scoped to one Assistant batch and may be
                // reused later. Unanswered calls from an earlier batch were
                // never executed, so they must not survive this boundary.
                pending_mutations.clear();
                let Some(tool_calls) = message.tool_calls.as_ref() else {
                    continue;
                };
                for tool_call in tool_calls {
                    let Some(function) = tool_call.function.as_ref() else {
                        continue;
                    };
                    let Ok(args) = serde_json::from_str::<serde_json::Value>(&function.arguments) else {
                        continue;
                    };
                    if !vtcode_core::tools::tool_intent::classify_tool_intent(&function.name, &args).mutating {
                        continue;
                    }
                    let paths = vtcode_core::tools::mutation_target_paths(&function.name, &args);
                    if !paths.is_empty() {
                        pending_mutations.insert(tool_call.id.clone(), paths);
                    }
                }
            }
            uni::MessageRole::Tool => {
                let Some(call_id) = message.tool_call_id.as_deref() else {
                    continue;
                };
                let Some(paths) = pending_mutations.remove(call_id) else {
                    continue;
                };
                if tool_response_is_success(message)
                    && paths.iter().any(|path| {
                        vtcode_core::tools::code_search_scope_contains_mutated_path(search_args, path, workspace_root)
                    })
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn tool_response_is_success(message: &uni::Message) -> bool {
    let Ok(output) = serde_json::from_str::<serde_json::Value>(&message.content.as_text()) else {
        return false;
    };
    let Some(output) = output.as_object() else {
        return false;
    };
    if output.contains_key("error") || output.contains_key("error_type") || output.contains_key("failure_kind") {
        return false;
    }
    if output.get("status").is_some_and(|status| status.as_str() != Some("success")) {
        return false;
    }

    match output.get("success") {
        Some(serde_json::Value::Bool(success)) => *success,
        Some(_) => false,
        None => output
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| status == "success"),
    }
}

fn output_has_empty_search_results(output: &serde_json::Value) -> bool {
    output
        .get("results")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|results| results.is_empty())
        && !output_has_actionable_recovery_guidance(output)
        && !output_has_error_signal(output)
}

fn output_has_actionable_recovery_guidance(output: &serde_json::Value) -> bool {
    ["hint", "next_action", "critical_note", "warning"].iter().any(|key| {
        output
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    }) || output
        .get("fallback_tool")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        || output.get("hints").and_then(serde_json::Value::as_array).is_some_and(|hints| {
            hints
                .iter()
                .any(|hint| hint.as_str().is_some_and(|value| !value.trim().is_empty()))
        })
}

fn output_has_error_signal(output: &serde_json::Value) -> bool {
    ["error", "error_type", "stderr", "stderr_preview", "message"]
        .iter()
        .any(|key| !output_field_is_empty(output.get(*key)))
}

fn output_reuses_recent_result(output: &serde_json::Value) -> bool {
    [
        "loop_detected",
        "reused_recent_result",
        "spool_ref_only",
        "result_ref_only",
    ]
    .iter()
    .any(|key| output.get(*key).and_then(serde_json::Value::as_bool) == Some(true))
}

fn error_is_missing_resource(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "not found",
        "no such file",
        "resource not found",
        "spool file not found",
        "session output file not found",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_low_signal_outcome(outcome: &ToolPipelineOutcome, canonical_tool_name: &str, args: &serde_json::Value) -> bool {
    match &outcome.status {
        ToolExecutionStatus::Success { output, command_success, .. } => {
            output_has_empty_search_results(output)
                || output_reuses_recent_result(output)
                || (matches!(
                    canonical_tool_name,
                    vtcode_core::config::constants::tools::UNIFIED_EXEC
                        | vtcode_core::config::constants::tools::EXEC_COMMAND
                ) && !*command_success
                    && is_grep_style_no_match(canonical_tool_name, args, output))
        }
        ToolExecutionStatus::Failure { error } => error_is_missing_resource(&error.message),
        ToolExecutionStatus::Timeout { .. } | ToolExecutionStatus::Cancelled => false,
    }
}

/// Upsert a tool result into `history`, keyed on `tool_call_id`.
///
/// This is a **bounded** upsert: the reverse scan stops as soon as it reaches
/// ANY Assistant message (regardless of its tool_calls). This is critical:
/// Assistant messages represent turn boundaries. Tool responses from before an
/// Assistant must never be overwritten by Tool responses from after it, even
/// when fabricated tool_call_ids collide across turns.
///
/// If a Tool message with a matching id is found *before* the nearest
/// Assistant boundary, it is a legitimate same-call update (e.g. an
/// auto-permission probe replaying a result) and gets overwritten in place.
/// If the boundary is hit first, the id has been reused across turns, so we
/// append instead of clobbering an unrelated, earlier Tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolResponseHistoryUpdate {
    Appended,
    Replaced { previous_text_len: usize },
}

pub(crate) fn push_tool_response<S>(
    history: &mut Vec<uni::Message>,
    tool_call_id: S,
    tool_name: Option<&str>,
    content: String,
) -> ToolResponseHistoryUpdate
where
    S: AsRef<str> + Into<String>,
{
    let tool_call_id_ref = tool_call_id.as_ref();
    let mut overwrite_index = None;
    for (index, message) in history.iter().enumerate().rev() {
        match message.role {
            uni::MessageRole::Tool => {
                if message.tool_call_id.as_deref() == Some(tool_call_id_ref) {
                    overwrite_index = Some(index);
                    break;
                }
            }
            // Stop at ANY Assistant message — it marks a turn boundary.
            // Tool responses from before this Assistant must not be overwritten.
            uni::MessageRole::Assistant => {
                break;
            }
            _ => {}
        }
    }

    if let Some(index) = overwrite_index {
        let previous_text_len = history[index].content.as_text().len();
        history[index].content = uni::MessageContent::Text(content);
        if let Some(tool_name) = tool_name {
            history[index].origin_tool = Some(tool_name.to_string());
        }
        return ToolResponseHistoryUpdate::Replaced { previous_text_len };
    }

    let tool_call_id = tool_call_id.into();
    history.push(match tool_name {
        Some(name) => uni::Message::tool_response_with_origin(tool_call_id, content, name.to_string()),
        None => uni::Message::tool_response(tool_call_id, content),
    });
    ToolResponseHistoryUpdate::Appended
}

/// Generate a tool signature key with predictable structure for loop tracking.
pub(crate) fn signature_key_for(name: &str, args: &serde_json::Value) -> String {
    // Keep keys compact on hot paths: hash bounded argument bytes instead of
    // allocating full JSON payloads for large tool arguments.
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut input_len = 0usize;
    let mutability_tag = if vtcode_core::tools::tool_intent::classify_tool_intent(name, args).mutating {
        "rw"
    } else {
        "ro"
    };

    if serde_json::to_writer(HashingWriter::new(&mut hash, &mut input_len), args).is_err() {
        for byte in b"{}" {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
            input_len = input_len.saturating_add(1);
        }
    }

    format!("{name}:{mutability_tag}:len{input_len}-fnv{hash:016x}")
}

/// Generate a read-normalized signature key for cross-turn dedup.
///
/// File-read tools (`file_operation` with `read` action, `read_file`,
/// `grep_file`, `list_files`) omit pagination and read-offset fields so that
/// re-reading the same target groups under one logical read. `code_search`
/// uses its normalized result-replay identity, which preserves the effective
/// `max_results`; its separate loop identity may group searches across limits.
///
/// For mutating tools the original `signature_key_for` is returned unchanged.
pub(crate) fn read_normalized_signature_key(name: &str, args: &serde_json::Value) -> String {
    if name == vtcode_core::config::constants::tools::CODE_SEARCH
        && let Some(identity) = vtcode_core::tools::normalized_code_search_identity(args)
    {
        return format!("{name}:ro:{identity}");
    }

    if !is_read_only_tool_args(name, args) {
        return signature_key_for(name, args);
    }

    let Some(mut obj) = args.as_object().cloned() else {
        return signature_key_for(name, args);
    };

    // Strip pagination / read-offset fields that don't change *what* is read.
    for key in read_extent::normalization_strip_keys() {
        obj.remove(key);
    }

    let normalized = serde_json::Value::Object(obj);
    signature_key_for(name, &normalized)
}

/// Returns `true` when `(name, args)` describe a read-only tool invocation.
fn is_read_only_tool_args(name: &str, args: &serde_json::Value) -> bool {
    use vtcode_core::config::constants::tools;
    match name {
        tools::READ_FILE | tools::GREP_FILE | tools::LIST_FILES => true,
        tools::CODE_SEARCH => true,
        tools::UNIFIED_SEARCH | "search_dispatch" => true,
        tools::UNIFIED_FILE | "file_operation" => {
            matches!(args.get("action").and_then(|v| v.as_str()), Some("read"))
        }
        _ => false,
    }
}

struct HashingWriter<'a> {
    hash: &'a mut u64,
    input_len: &'a mut usize,
}

impl<'a> HashingWriter<'a> {
    fn new(hash: &'a mut u64, input_len: &'a mut usize) -> Self {
        Self { hash, input_len }
    }
}

impl std::io::Write for HashingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for byte in buf {
            *self.hash ^= u64::from(*byte);
            *self.hash = self.hash.wrapping_mul(0x100000001b3);
            *self.input_len = self.input_len.saturating_add(1);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn resolve_max_tool_retries(
    _tool_name: &str,
    vt_cfg: Option<&vtcode_core::config::loader::VTCodeConfig>,
) -> usize {
    vt_cfg
        .map(|cfg| cfg.agent.harness.max_tool_retries as usize)
        .unwrap_or(vtcode_config::constants::defaults::DEFAULT_MAX_TOOL_RETRIES as usize)
}

fn path_targets_plan_artefact(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    normalized == ".vtcode/plans"
        || normalized.starts_with(".vtcode/plans/")
        || normalized.contains("/.vtcode/plans/")
        || normalized == "/tmp/vtcode-plans"
        || normalized.starts_with("/tmp/vtcode-plans/")
        || normalized.contains("/tmp/vtcode-plans/")
}

pub(crate) fn is_plan_artefact_write(name: &str, args: &serde_json::Value) -> bool {
    use vtcode_core::config::constants::tools as tool_names;
    use vtcode_core::tools::names::canonical_tool_name;
    use vtcode_core::tools::tool_intent::file_operation_action;

    let canonical = canonical_tool_name(name);
    match canonical {
        tool_names::TASK_TRACKER => true,
        tool_names::UNIFIED_FILE => {
            if !file_operation_action(args)
                .map(|action| action.eq_ignore_ascii_case("read"))
                .unwrap_or(false)
            {
                [
                    "path",
                    "file_path",
                    "filepath",
                    "filePath",
                    "target_path",
                    "destination",
                    "destination_path",
                ]
                .iter()
                .filter_map(|key| args.get(*key).and_then(|value| value.as_str()))
                .any(path_targets_plan_artefact)
            } else {
                false
            }
        }
        tool_names::WRITE_FILE | tool_names::EDIT_FILE | tool_names::CREATE_FILE | tool_names::SEARCH_REPLACE => {
            ["path", "file_path", "filepath", "filePath"]
                .iter()
                .filter_map(|key| args.get(*key).and_then(|value| value.as_str()))
                .any(path_targets_plan_artefact)
        }
        _ => false,
    }
}

fn is_execution_tool(name: &str) -> bool {
    use vtcode_core::config::constants::tools as tool_names;

    matches!(
        name,
        tool_names::UNIFIED_EXEC
            | tool_names::EXEC_COMMAND
            | tool_names::EXEC_PTY_CMD
            | tool_names::RUN_PTY_CMD
            | tool_names::EXECUTE_CODE
            | tool_names::SHELL
    )
}

/// Return whether a tool call must wait for a successful verification step.
///
/// Reads, inspections, verification commands, task tracking, and dedicated
/// plan-artefact writes remain available while the checkpoint is pending.
/// A failed verifier grants a bounded fix-up window ([`FAILED_VERIFICATION_FIX_ALLOWANCE`])
/// so a broken build can be repaired, and piped verifier attempts
/// (e.g. `cargo check 2>&1 | head`) are admitted to run even though only a
/// standalone success clears the gate.
pub(crate) fn mutation_blocked_until_verification(
    loop_tracker: &LoopTracker,
    name: &str,
    args: &serde_json::Value,
) -> bool {
    if !loop_tracker.verification_is_pending() || is_plan_artefact_write(name, args) {
        return false;
    }

    let canonical_name = canonical_tool_name(name);
    if is_execution_tool(canonical_name) {
        // Truncation-only verifier attempts (`cargo check 2>&1 | head`) must
        // run so the model can see the failure; they never clear the gate
        // (see update_repetition_tracker). The admission predicate requires
        // every shell segment to be verification-or-readonly, so a smuggled
        // mutation such as `cargo check && rm -rf target` stays blocked.
        if shell_command_is_admitted_verification_attempt(args)
            && matches!(classify_shell_activity(canonical_name, args), ShellActivity::Mutation)
        {
            return false;
        }
        if !matches!(classify_shell_activity(canonical_name, args), ShellActivity::Mutation) {
            return false;
        }
        // Fix-up window: allow bounded repair edits after a failed verifier.
        return loop_tracker.fix_edits_remaining == 0;
    }

    if !vtcode_core::tools::tool_intent::classify_tool_intent(canonical_name, args).mutating {
        return false;
    }
    loop_tracker.fix_edits_remaining == 0
}

/// Updates the tool repetition tracker based on the execution outcome.
///
/// Count completed attempts for repetition detection, but only successful
/// mutations contribute to anti-blind-editing verification pressure.
pub(crate) fn update_repetition_tracker(
    loop_tracker: &mut LoopTracker,
    outcome: &ToolPipelineOutcome,
    name: &str,
    args: &serde_json::Value,
) {
    if matches!(&outcome.status, ToolExecutionStatus::Cancelled) {
        return;
    }

    let canonical_name = canonical_tool_name(name);
    let signature_key = signature_key_for(canonical_name, args);
    loop_tracker.record(signature_key.clone());
    let low_signal_family =
        crate::agent::runloop::unified::turn::tool_outcomes::handlers::low_signal_family_key(canonical_name, args)
            .filter(|_| is_low_signal_outcome(outcome, canonical_name, args));
    let is_low_signal_navigation = low_signal_family.is_some();
    if let Some(low_signal_family) = low_signal_family.as_ref() {
        loop_tracker.record_low_signal(low_signal_family.clone());
    }

    // Update NL2Repo-Bench metrics based on tool intent.
    //
    // IMPORTANT: Check execution tools FIRST. `classify_tool_intent` marks
    // `command_session(action=run)` as `mutating: true` because shell commands *can*
    // mutate state, but for the Edit-Test heuristic, any execution/verification
    // step (cargo check, cargo test, etc.) should RESET the mutation counter,
    // not increment it.
    if is_execution_tool(canonical_name) {
        match classify_shell_activity(canonical_name, args) {
            ShellActivity::Inspection => {
                loop_tracker.consecutive_navigations = loop_tracker.consecutive_navigations.saturating_add(1);
                loop_tracker.nav_signatures.insert(signature_key);
                loop_tracker.record_navigation_signal(is_low_signal_navigation);
            }
            ShellActivity::Verification => {
                if matches!(&outcome.status, ToolExecutionStatus::Success { command_success: true, .. }) {
                    loop_tracker.mark_verification_complete();
                } else if matches!(&outcome.status, ToolExecutionStatus::Success { command_success: false, .. }) {
                    // Only a verifier that actually ran and reported non-zero
                    // opens the fix-up window. Tool-level Failure/Timeout (never
                    // executed, e.g. argument errors) must not grant edits.
                    loop_tracker.record_failed_verification();
                }
                loop_tracker.reset_navigation_window(low_signal_family.is_none());
            }
            ShellActivity::Mutation => {
                // Truncation-only verifier attempts (e.g. `cargo check 2>&1 | head`)
                // are admitted to run but never clear the gate: the pipeline
                // exit status is the truncator's, not the verifier's. Don't
                // count them as blind edits; a failed piped attempt still
                // opens the fix window so the agent can repair and re-run a
                // standalone verifier. Chained mutations smuggled behind a
                // verifier prefix are rejected by the admission predicate and
                // take the blind-edit path below.
                if shell_command_is_admitted_verification_attempt(args) {
                    let ran_and_failed =
                        matches!(&outcome.status, ToolExecutionStatus::Success { command_success: false, .. });
                    if ran_and_failed {
                        loop_tracker.record_failed_verification();
                    }
                    loop_tracker.reset_navigation_window(low_signal_family.is_none());
                } else {
                    if mutation_was_applied(outcome) {
                        loop_tracker.record_successful_mutation();
                    }
                    loop_tracker.reset_navigation_window(low_signal_family.is_none());
                }
            }
        }
    } else if is_plan_artefact_write(canonical_name, args) {
        // Plan artefact writes in dedicated plan storage are allowed in Planning workflow and
        // should not trigger anti-blind-editing verification pressure.
        // Low-signal repetition history is preserved: plan writes are not
        // navigation, so they neither advance nor clear that window.
        loop_tracker.reset_navigation_window(false);
    } else {
        let intent = vtcode_core::tools::tool_intent::classify_tool_intent(canonical_name, args);
        if intent.mutating {
            if mutation_was_applied(outcome) {
                loop_tracker.record_successful_mutation();
            }
            loop_tracker.reset_navigation_window(low_signal_family.is_none());
        } else {
            // Read-only / navigation tool
            loop_tracker.consecutive_navigations += 1;
            loop_tracker.nav_signatures.insert(signature_key);
            loop_tracker.record_navigation_signal(is_low_signal_navigation);
        }
    }
}

fn mutation_was_applied(outcome: &ToolPipelineOutcome) -> bool {
    match &outcome.status {
        ToolExecutionStatus::Success { output, command_success, modified_files, .. } => {
            if let Some(effective_change) = vtcode_core::tools::file_ops::diff_output_has_effective_change(output) {
                return effective_change;
            }
            *command_success || !modified_files.is_empty()
        }
        ToolExecutionStatus::Failure { .. } | ToolExecutionStatus::Timeout { .. } | ToolExecutionStatus::Cancelled => {
            false
        }
    }
}
pub(crate) fn serialize_output(output: &serde_json::Value) -> String {
    if let Some(s) = output.as_str() {
        s.to_string()
    } else {
        serde_json::to_string(output).unwrap_or_else(|_| "{}".to_string())
    }
}

pub(crate) fn check_is_argument_error(error_str: &str) -> bool {
    error_str.contains("Missing required")
        || error_str.contains("Invalid arguments")
        || error_str.contains("Tool argument validation failed")
        || error_str.contains("required path parameter")
        || error_str.contains("is required for '")
        || error_str.contains("is required for \"")
        || error_str.contains("'index' is required")
        || error_str.contains("'index_path' is required")
        || error_str.contains("'status' is required")
        || error_str.contains("expected ")
        || error_str.contains("Expected:")
}

#[cfg(test)]
#[path = "helpers_tests/mod.rs"]
mod tests;
