use std::borrow::Cow;
use std::sync::Arc;

use anyhow::Result;
use diapason_core::config::constants::tools as tool_names;
use diapason_core::tools::validation_cache::ValidationCache;
use diapason_core::utils::ansi::MessageStyle;
use serde_json::Value;

use crate::agent::runloop::unified::turn::context::{TurnHandlerOutcome, TurnLoopResult, TurnProcessingContext};

/// Validates that a textual tool call has required arguments before execution.
/// Returns `None` if valid, or `Some(missing_params)` if validation fails.
///
/// This prevents executing tools with empty args that will just fail,
/// allowing the Model to continue naturally instead of hitting loop detection.
/// Validates that a textual tool call has required arguments and passes security checks.
/// Returns `None` if valid, or `Some(failures)` if validation fails.
///
/// Optimization: Uses static slices for required params to avoid allocations
pub(crate) fn validate_tool_args_security(
    name: &str,
    args: &Value,
    validation_cache: Option<&Arc<ValidationCache>>,
    tool_registry: Option<&diapason_core::tools::ToolRegistry>,
) -> Option<Vec<String>> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::io;

    use diapason_core::tools::validation::{commands, paths};

    struct HasherWriter<'a, H: Hasher>(&'a mut H);
    impl<H: Hasher> io::Write for HasherWriter<'_, H> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // Calculate hash for caching
    let args_hash = if validation_cache.is_some() {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        if serde_json::to_writer(HasherWriter(&mut hasher), args).is_err() {
            // Fallback path should be rare; keep it resilient.
            args.to_string().hash(&mut hasher);
        }
        Some(hasher.finish())
    } else {
        None
    };

    // Check cache
    if let Some(hash) = args_hash
        && let Some(cache) = validation_cache
    {
        // ValidationCache has interior mutability, use directly
        if let Some(is_valid) = cache.check(name, hash)
            && is_valid
        {
            return None; // Valid cached
        }
        // If invalid (false), we continue to re-validate to generate error messages
    }

    if let Some(registry) = tool_registry {
        match registry.admit_public_tool_call(name, args) {
            Ok(_) => {
                if let Some(hash) = args_hash
                    && let Some(cache) = validation_cache
                {
                    cache.insert(name, hash, true);
                }
                return None;
            }
            Err(err) => {
                return Some(vec![err.to_string()]);
            }
        }
    }

    use diapason_core::config::constants::tools as tool_names;

    fn is_missing_arg_value(args: &Value, key: &str) -> bool {
        match args.get(key) {
            Some(v) => v.is_null() || (v.is_string() && v.as_str().is_none_or(|s| s.trim().is_empty())),
            None => true,
        }
    }

    fn is_missing_required_arg(tool_name: &str, args: &Value, key: &str) -> bool {
        if tool_name == tool_names::EDIT_FILE {
            return match key {
                "old_str" => is_missing_arg_value(args, "old_str") && is_missing_arg_value(args, "old_string"),
                "new_str" => is_missing_arg_value(args, "new_str") && is_missing_arg_value(args, "new_string"),
                _ => is_missing_arg_value(args, key),
            };
        }

        is_missing_arg_value(args, key)
    }

    // Optimization: Early return for tools with no requirements
    static EMPTY_REQUIRED: &[&str] = &[];

    // 1. Check required arguments using static slices
    let required: &[&str] = match name {
        tool_names::READ_FILE => &["path"],
        tool_names::WRITE_FILE => &["path", "content"],
        tool_names::EDIT_FILE => &["path", "old_str", "new_str"],
        tool_names::LIST_FILES => &["path"],
        tool_names::GREP_FILE => &["pattern", "path"],
        tool_names::RUN_PTY_CMD => &["command"],
        tool_names::APPLY_PATCH => &["patch"],
        _ => EMPTY_REQUIRED,
    };

    // Optimization: Pre-allocate failures vec only when needed
    let mut failures: Option<Vec<String>> = None;

    if !required.is_empty() {
        for key in required {
            if is_missing_required_arg(name, args, key) {
                failures
                    .get_or_insert_with(|| Vec::with_capacity(required.len()))
                    .push(format!("Missing required argument: {key}"));
            }
        }
    }
    if name == tool_names::UNIFIED_EXEC {
        let exec_failures = diapason_core::tools::command_args::command_session_missing_required_args(args);
        if !exec_failures.is_empty() {
            failures
                .get_or_insert_with(|| Vec::with_capacity(exec_failures.len()))
                .extend(exec_failures.into_iter().map(|key| format!("Missing required argument: {key}")));
        }
    }

    // Early return if required args are missing
    if failures.is_some() {
        // Validation failed, no cache update (or cache as invalid if we wanted)
        return failures;
    }

    if name == tool_names::UNIFIED_EXEC && diapason_core::tools::tool_intent::command_session_action(args).is_none() {
        return Some(vec![
            "Invalid arguments: missing action; provide `action` or inferable exec arguments".to_string(),
        ]);
    }

    // 2. Perform security checks only if required args passed
    // Path safety checks
    if let Some(path) = args.get("path").and_then(|v| v.as_str())
        && let Err(e) = paths::validate_path_safety(path)
    {
        failures
            .get_or_insert_with(|| Vec::with_capacity(2))
            .push(format!("Path security check failed: {e}"));
    }

    // Command safety checks
    if (name == tool_names::RUN_PTY_CMD
        || (name == tool_names::UNIFIED_EXEC
            && diapason_core::tools::command_args::command_session_requires_command_safety(args)))
        && let Some(cmd) = diapason_core::tools::command_args::command_text(args).ok().flatten()
        && let Err(e) = commands::validate_command_safety(&cmd)
    {
        failures
            .get_or_insert_with(|| Vec::with_capacity(2))
            .push(format!("Command security check failed: {e}"));
    }

    // Update cache if valid
    if failures.is_none()
        && let Some(hash) = args_hash
        && let Some(cache) = validation_cache
    {
        // ValidationCache has interior mutability
        cache.insert(name, hash, true);
    }

    failures
}

pub(crate) async fn run_proactive_guards(ctx: &mut TurnProcessingContext<'_>, _step_count: usize) -> Result<()> {
    // Auto-prune decision ledgers to prevent unbounded memory growth
    {
        let mut decision_ledger = ctx.decision_ledger.write().await;
        decision_ledger.auto_prune();
    }

    // Context trim and compaction has been removed - no proactive guards needed
    // The function is kept for future extensibility but now does minimal work

    Ok(())
}

/// Check if a tool signature represents a read-only operation
/// Signature format: "tool_name:args_json" where args_json is serialized Value
fn is_readonly_signature(signature: &str) -> bool {
    if let Some(first_colon) = signature.find(':')
        && let Some(second_colon_rel) = signature[first_colon + 1..].find(':')
    {
        let tag_start = first_colon + 1;
        let tag_end = tag_start + second_colon_rel;
        match &signature[tag_start..tag_end] {
            "ro" => return true,
            "rw" => return false,
            _ => {}
        }
    }

    // Prefer `:{` / `:[` separators so tool names containing `::` don't break parsing.
    let colon_pos = signature
        .find(":{")
        .or_else(|| signature.find(":["))
        .or_else(|| signature.find(':'));
    let Some(colon_pos) = colon_pos else {
        return false;
    };
    let tool_name = normalize_turn_balancer_tool_name(&signature[..colon_pos]);
    let args_json = &signature[colon_pos + 1..];

    let tool_name_str: &str = tool_name.as_ref();

    if let Ok(args) = serde_json::from_str::<Value>(args_json) {
        return !diapason_core::tools::tool_intent::classify_tool_intent(tool_name_str, &args).mutating;
    }

    // Fallback for malformed signature payloads.
    if matches!(
        tool_name_str,
        tool_names::READ_FILE
            | tool_names::GREP_FILE
            | tool_names::LIST_FILES
            | tool_names::CODE_SEARCH
            | "search_tools"
            | "agent_info"
    ) {
        return true;
    }

    if tool_name_str == tool_names::UNIFIED_FILE {
        let lower_json = args_json.to_ascii_lowercase();
        return lower_json.contains(r#""action":"read""#)
            || lower_json.contains(r#""action": "read""#)
            || lower_json.contains(r#"'action':'read'"#);
    }
    if tool_name_str == tool_names::UNIFIED_EXEC {
        let lower_json = args_json.to_ascii_lowercase();
        return lower_json.contains(r#""action":"poll""#)
            || lower_json.contains(r#""action":"list""#)
            || lower_json.contains(r#""action":"inspect""#)
            || lower_json.contains(r#""action":"run""#)
            || lower_json.contains(r#""action": "poll""#)
            || lower_json.contains(r#""action": "list""#)
            || lower_json.contains(r#""action": "inspect""#)
            || lower_json.contains(r#""action": "run""#);
    }

    false
}

fn normalize_turn_balancer_tool_name(name: &str) -> Cow<'_, str> {
    let lowered = name.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "read file" | "repo_browser.read_file" => Cow::Borrowed(tool_names::READ_FILE),
        "write file" | "repo_browser.write_file" => Cow::Borrowed(tool_names::WRITE_FILE),
        "edit file" => Cow::Borrowed(tool_names::EDIT_FILE),
        "code search" | "search code" => Cow::Borrowed(tool_names::CODE_SEARCH),
        "search text"
        | "list files"
        | "structural search"
        | "code intelligence"
        | "list tools"
        | "list errors"
        | "show agent info"
        | "fetch"
        | "search_dispatch"
        | "search_dispatch_internal" => Cow::Borrowed(tool_names::UNIFIED_SEARCH),
        "run command (pty)"
        | "run command"
        | "run code"
        | "exec code"
        | "bash"
        | "command_session"
        | "container.exec"
        | "command_session_internal" => Cow::Borrowed(tool_names::UNIFIED_EXEC),
        "apply patch"
        | "delete file"
        | "move file"
        | "copy file"
        | "file operation"
        | "file_operation"
        | "file_operation_internal" => Cow::Borrowed(tool_names::UNIFIED_FILE),
        _ => Cow::Owned(lowered),
    }
}

fn navigation_loop_guidance(planning_active: bool, repetition: usize) -> &'static str {
    if repetition >= 2 {
        "CRITICAL: You have triggered the navigation-loop guard repeatedly. STOP all read/search operations immediately. DO NOT browse or explore further. Provide a direct synthesis with the next action or ask one blocking question, and nothing else."
    } else if planning_active {
        "WARNING: Too many read/search steps in Planning workflow without an actionable output. Stop browsing, summarize key findings, then update `task_tracker` with concrete steps (files + outcome + verification), or ask one blocking question."
    } else {
        "WARNING: Too many read/search steps without edits or execution. Summarize findings and propose the next concrete edit/action, or explain the blocker."
    }
}

pub(crate) async fn handle_turn_balancer(
    ctx: &mut TurnProcessingContext<'_>,
    step_count: usize,
    repeated_tool_attempts: &mut crate::agent::runloop::unified::turn::tool_outcomes::helpers::LoopTracker,
    max_tool_loops: usize,
    tool_repeat_limit: usize,
) -> TurnHandlerOutcome {
    use diapason_core::llm::provider as uni;

    use crate::agent::runloop::unified::turn::tool_outcomes::helpers::{
        ANTI_BLIND_EDITING_DIRECTIVE, ANTI_BLIND_EDITING_WARNING, NAVIGATION_LOOP_THRESHOLD,
        PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD, PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD,
    };

    // NL2Repo-Bench checks run on every step (no backoff) since they
    // are safety guardrails, not performance optimizations.

    // NL2Repo-Bench: Edit-Test Validation Loop (Anti-Blind-Editing)
    if repeated_tool_attempts.verification_is_pending() {
        repeated_tool_attempts.mark_verification_pending();
        if !repeated_tool_attempts.verification_warning_emitted {
            ctx.renderer
                .line(MessageStyle::Warning, ANTI_BLIND_EDITING_WARNING)
                .unwrap_or(());
            ctx.working_history
                .push(uni::Message::system(ANTI_BLIND_EDITING_DIRECTIVE.to_string()));
            repeated_tool_attempts.verification_warning_emitted = true;
        }
        return TurnHandlerOutcome::Continue;
    }

    // Planning keeps its generous 120-call ceiling for genuinely complex
    // research, but diverse empty searches and ref-only reads must converge
    // much earlier. Trigger a single tool-free synthesis pass at the first
    // adaptive threshold; recovery resets both turn-scoped counters.
    if ctx.is_planning_active()
        && !repeated_tool_attempts.planning_low_signal_synthesis_triggered
        && (repeated_tool_attempts.consecutive_low_signal_navigations >= PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD
            || repeated_tool_attempts.total_low_signal_navigations >= PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD)
    {
        repeated_tool_attempts.planning_low_signal_synthesis_triggered = true;
        let recovery_reason = format!(
            "Planning navigation produced {} consecutive and {} total low-signal results. Tools are disabled on the next pass; synthesize the plan from collected evidence.",
            repeated_tool_attempts.consecutive_low_signal_navigations,
            repeated_tool_attempts.total_low_signal_navigations
        );
        ctx.activate_recovery(recovery_reason.clone());
        ctx.renderer
            .line(
                MessageStyle::Info,
                "[!] Planning recovery: low-signal research reached the adaptive synthesis threshold.",
            )
            .unwrap_or(());
        ctx.working_history.push(uni::Message::system(recovery_reason));
        return apply_balancer_recovery(repeated_tool_attempts);
    }

    // NL2Repo-Bench: Navigation Loop Detection
    // Only trigger when there are actual repeated navigations (not just diverse exploration).
    // Reading 15 different files is exploration; re-reading the same 3 files 5x each is a loop.
    if repeated_tool_attempts.consecutive_navigations >= NAVIGATION_LOOP_THRESHOLD
        && repeated_tool_attempts.repeated_navigation_count() >= 3
    {
        repeated_tool_attempts.navigation_loop_recoveries =
            repeated_tool_attempts.navigation_loop_recoveries.saturating_add(1);
        let recurrence = repeated_tool_attempts.navigation_loop_recoveries;
        if ctx.is_approved_plan_execution() && recurrence >= 2 {
            let blocker = "Approved-plan execution stopped after repeated read-only/navigation actions made no implementation progress. The approved plan and task checklist were retained; retry from the pending step.";
            ctx.renderer.line(MessageStyle::Warning, blocker).unwrap_or(());
            ctx.working_history.push(uni::Message::system(blocker.to_string()));
            return TurnHandlerOutcome::Break(TurnLoopResult::Blocked { reason: Some(blocker.to_string()) });
        }
        let recovery_reason = format!(
            "Navigation loop detected after {} consecutive read/search steps (recurrence #{recurrence}). Tools are disabled on the next pass; summarize findings and propose the next concrete action.",
            repeated_tool_attempts.consecutive_navigations
        );
        ctx.activate_recovery(recovery_reason.clone());
        ctx.renderer
            .line(MessageStyle::Warning, "[!] Navigation Loop: scheduling a recovery synthesis pass.")
            .unwrap_or(());
        ctx.working_history.push(uni::Message::system(format!(
            "{} {}",
            recovery_reason,
            navigation_loop_guidance(ctx.is_planning_active(), recurrence)
        )));
        return apply_balancer_recovery(repeated_tool_attempts);
    }

    // --- Turn balancer: cap low-signal churn ---
    // Optimization: Skip with exponential backoff to reduce iteration frequency
    let check_interval = if step_count <= 4 {
        1
    } else {
        1_usize << ((step_count / 4).ilog2())
    };

    let effective_repeat_limit = tool_repeat_limit.max(3);
    let repeated_low_signal = repeated_tool_attempts.max_low_signal_count();
    if repeated_low_signal >= effective_repeat_limit
        && repeated_tool_attempts.consecutive_navigations >= effective_repeat_limit
    {
        let recovery_reason = format!(
            "Repeated low-signal navigation calls reached the per-turn fast-path cap ({effective_repeat_limit}). Tools are disabled on the next pass; summarize only from collected evidence."
        );
        ctx.activate_recovery(recovery_reason.clone());
        ctx.renderer
            .line(
                MessageStyle::Info,
                "[!] Turn balancer: repeated low-signal navigation detected; scheduling an early recovery pass.",
            )
            .unwrap_or(());
        ctx.working_history.push(uni::Message::system(recovery_reason));
        {
            let mut ledger = ctx.decision_ledger.write().await;
            ledger.record_decision(
                "Turn balancer: Early recovery intervention".to_string(),
                diapason_core::core::decision_tracker::Action::Response {
                    content:
                        "Repeated low-signal navigation was detected; an early tool-free recovery pass was scheduled."
                            .to_string(),
                    response_type: diapason_core::core::decision_tracker::ResponseType::ContextSummary,
                },
                None,
            );
        }
        return apply_balancer_recovery(repeated_tool_attempts);
    }

    if !step_count.is_multiple_of(check_interval) {
        return TurnHandlerOutcome::Continue;
    }

    // Exclude read-only tools from repeated count (they're legitimate exploration)
    let max_repeated = repeated_tool_attempts
        .max_count_filtered(is_readonly_signature)
        .max(repeated_low_signal);

    if crate::agent::runloop::unified::turn::utils::should_trigger_turn_balancer(
        step_count,
        max_tool_loops,
        max_repeated,
        tool_repeat_limit,
    ) {
        let recovery_reason =
            "Turn balancer detected repeated low-signal tool churn. Tools are disabled on the next pass; summarize only from collected evidence.".to_string();
        ctx.activate_recovery(recovery_reason.clone());
        ctx.renderer
            .line(
                MessageStyle::Info,
                "[!] Turn balancer: repeated low-signal calls detected; scheduling a final recovery pass.",
            )
            .unwrap_or(());
        ctx.working_history.push(uni::Message::system(recovery_reason));
        // Record in ledger
        {
            let mut ledger = ctx.decision_ledger.write().await;
            ledger.record_decision(
                "Turn balancer: Recovery intervention".to_string(),
                diapason_core::core::decision_tracker::Action::Response {
                    content: "Low-signal churn detected; a final tool-free recovery pass was scheduled.".to_string(),
                    response_type: diapason_core::core::decision_tracker::ResponseType::ContextSummary,
                },
                None,
            );
        }
        return apply_balancer_recovery(repeated_tool_attempts);
    }

    TurnHandlerOutcome::Continue
}

fn apply_balancer_recovery(
    repeated_tool_attempts: &mut crate::agent::runloop::unified::turn::tool_outcomes::helpers::LoopTracker,
) -> TurnHandlerOutcome {
    repeated_tool_attempts.reset_after_balancer_recovery();
    TurnHandlerOutcome::Continue
}

#[cfg(test)]
mod tests {
    use diapason_core::config::constants::tools as tool_names;
    use serde_json::json;

    use super::{
        apply_balancer_recovery, is_readonly_signature, navigation_loop_guidance, validate_tool_args_security,
    };
    use crate::agent::runloop::unified::tool_pipeline::{ToolExecutionStatus, ToolPipelineOutcome};
    use crate::agent::runloop::unified::turn::context::{TurnHandlerOutcome, TurnLoopResult};
    use crate::agent::runloop::unified::turn::tool_outcomes::helpers::{
        BLIND_EDITING_THRESHOLD, LoopTracker, NAVIGATION_LOOP_THRESHOLD, PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD,
        PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD, update_repetition_tracker,
    };
    use crate::agent::runloop::unified::turn::turn_processing::test_support::TestTurnProcessingBacking;

    #[test]
    fn readonly_signature_handles_file_and_code_search_signatures() {
        assert!(is_readonly_signature(r#"read file:{"path":"README.md"}"#));
        assert!(is_readonly_signature(
            r#"code_search:{"query":"LLMStreamEvent","path":"crates/codegen/diapason-core/src/llm/providers/anthropic/api.rs"}"#
        ));
    }

    #[test]
    fn readonly_signature_treats_safe_exec_runs_as_readonly() {
        assert!(is_readonly_signature(r#"command_session:{"action":"run","command":"cargo check"}"#));
    }

    #[test]
    fn readonly_signature_fast_path_accepts_ro_tag() {
        assert!(is_readonly_signature("code_search:ro:{\"query\":\"Widget\"}"));
    }

    #[test]
    fn readonly_signature_fast_path_rejects_rw_tag() {
        assert!(!is_readonly_signature("file_operation:rw:len42-fnv1234abcd"));
    }

    #[test]
    fn validate_edit_file_args_accepts_legacy_old_new_string_keys() {
        let args = json!({
            "path": "src/lib.rs",
            "old_string": "before",
            "new_string": "after"
        });

        assert!(validate_tool_args_security(tool_names::EDIT_FILE, &args, None, None).is_none());
    }

    #[test]
    fn validate_edit_file_args_still_rejects_when_replacements_missing() {
        let args = json!({
            "path": "src/lib.rs"
        });

        let failures = validate_tool_args_security(tool_names::EDIT_FILE, &args, None, None).unwrap();
        assert!(failures.iter().any(|msg| msg.contains("old_str")));
        assert!(failures.iter().any(|msg| msg.contains("new_str")));
    }

    #[test]
    fn validate_command_session_args_without_registry_reports_single_missing_command() {
        let failures = validate_tool_args_security(tool_names::UNIFIED_EXEC, &json!({"action": "run"}), None, None)
            .expect("missing command should fail");

        assert_eq!(failures, vec!["Missing required argument: command".to_string()]);
    }

    #[test]
    fn validate_command_session_args_without_registry_rejects_missing_action() {
        let failures = validate_tool_args_security(tool_names::UNIFIED_EXEC, &json!({}), None, None)
            .expect("missing action should fail");

        assert_eq!(
            failures,
            vec!["Invalid arguments: missing action; provide `action` or inferable exec arguments".to_string()]
        );
    }

    #[test]
    fn navigation_loop_guidance_mentions_task_tracker_in_planning_workflow() {
        let guidance = navigation_loop_guidance(true, 1);
        assert!(guidance.contains("task_tracker"));
    }

    #[test]
    fn navigation_loop_guidance_uses_generic_text_outside_planning_workflow() {
        let guidance = navigation_loop_guidance(false, 1);
        assert!(guidance.contains("read/search"));
        assert!(!guidance.contains("task_tracker"));
    }

    #[test]
    fn navigation_loop_guidance_escalates_on_repetition() {
        let guidance = navigation_loop_guidance(false, 2);
        assert!(guidance.contains("CRITICAL: You have triggered the navigation-loop guard repeatedly"));
    }

    #[test]
    fn balancer_recovery_continues_and_resets_tracker() {
        let mut tracker = LoopTracker::new();
        let sig = r#"command_session:{"action":"run","command":"cargo test"}"#.to_string();
        tracker.record(sig.clone());
        tracker.record(sig.clone());
        tracker.record(sig);
        tracker.consecutive_mutations = 3;
        tracker.consecutive_navigations = 5;

        let outcome = apply_balancer_recovery(&mut tracker);

        assert!(matches!(outcome, TurnHandlerOutcome::Continue));
        assert_eq!(tracker.max_count_filtered(|_| false), 0);
        assert_eq!(tracker.consecutive_mutations, 0);
        assert_eq!(tracker.consecutive_navigations, 0);
    }

    #[tokio::test]
    async fn planning_recovery_triggers_at_exact_consecutive_low_signal_threshold() {
        let mut backing = TestTurnProcessingBacking::new(120).await;
        backing.activate_planning_for_test();
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        tracker.consecutive_low_signal_navigations = PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD;
        tracker.total_low_signal_navigations = PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD;

        let outcome = super::handle_turn_balancer(&mut ctx, 6, &mut tracker, 120, 3).await;

        assert!(matches!(outcome, TurnHandlerOutcome::Continue));
        assert!(ctx.is_recovery_active());
        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
        assert_eq!(tracker.total_low_signal_navigations, 0);
        assert!(ctx.working_history.iter().any(|message| {
            message.content.as_text().contains("adaptive synthesis threshold")
                || message.content.as_text().contains("synthesize the plan")
        }));
    }

    #[tokio::test]
    async fn anti_blind_warning_keeps_verification_pending_until_a_check_runs() {
        let mut backing = TestTurnProcessingBacking::new(8).await;
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;

        let outcome = super::handle_turn_balancer(&mut ctx, 1, &mut tracker, 8, 3).await;

        assert!(matches!(outcome, TurnHandlerOutcome::Continue));
        assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
        assert!(
            ctx.working_history
                .iter()
                .any(|message| { message.content.as_text().contains("run `exec_command` to compile or test") })
        );
    }

    #[tokio::test]
    async fn planning_recovery_triggers_at_exact_total_low_signal_threshold() {
        let mut backing = TestTurnProcessingBacking::new(120).await;
        backing.activate_planning_for_test();
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        tracker.consecutive_low_signal_navigations = 1;
        tracker.total_low_signal_navigations = PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD;

        let outcome = super::handle_turn_balancer(&mut ctx, 10, &mut tracker, 120, 3).await;

        assert!(matches!(outcome, TurnHandlerOutcome::Continue));
        assert!(ctx.is_recovery_active());
        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
        assert_eq!(tracker.total_low_signal_navigations, 0);
    }

    #[tokio::test]
    async fn adaptive_low_signal_threshold_does_not_change_non_planning_turns() {
        let mut backing = TestTurnProcessingBacking::new(120).await;
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        tracker.consecutive_low_signal_navigations = PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD;
        tracker.total_low_signal_navigations = PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD;

        let outcome = super::handle_turn_balancer(&mut ctx, 1, &mut tracker, 120, 20).await;

        assert!(matches!(outcome, TurnHandlerOutcome::Continue));
        assert!(!ctx.is_recovery_active());
    }

    #[tokio::test]
    async fn adaptive_low_signal_synthesis_triggers_only_once_per_turn() {
        let mut backing = TestTurnProcessingBacking::new(120).await;
        backing.activate_planning_for_test();
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        tracker.consecutive_low_signal_navigations = PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD;
        tracker.total_low_signal_navigations = PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD;

        let first = super::handle_turn_balancer(&mut ctx, 6, &mut tracker, 120, 3).await;
        assert!(matches!(first, TurnHandlerOutcome::Continue));
        assert!(tracker.planning_low_signal_synthesis_triggered);

        tracker.consecutive_low_signal_navigations = PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD;
        tracker.total_low_signal_navigations = PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD;
        let second = super::handle_turn_balancer(&mut ctx, 12, &mut tracker, 120, usize::MAX).await;
        assert!(matches!(second, TurnHandlerOutcome::Continue));
        assert_eq!(tracker.consecutive_low_signal_navigations, PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD);
        assert_eq!(tracker.total_low_signal_navigations, PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD);
    }

    #[tokio::test]
    async fn navigation_loop_schedules_recovery_and_progress_only_recovery_text_completes() {
        let mut backing = TestTurnProcessingBacking::new(8).await;
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        tracker.consecutive_navigations = NAVIGATION_LOOP_THRESHOLD;

        let balancer_outcome = super::handle_turn_balancer(&mut ctx, 1, &mut tracker, 8, 3).await;
        assert!(matches!(balancer_outcome, TurnHandlerOutcome::Continue));
        assert_eq!(tracker.consecutive_navigations, 0);
        assert!(ctx.is_recovery_active());
        assert!(
            ctx.working_history
                .iter()
                .any(|message| { message.content.as_text().contains("Navigation loop detected") })
        );
        assert!(ctx.consume_recovery_pass());

        let recovery_outcome = ctx
            .handle_text_response(
                "I'll inspect one more file and then summarize.".to_string(),
                Vec::new(),
                None,
                None,
                false,
            )
            .await
            .expect("recovery response should be handled");

        assert!(matches!(recovery_outcome, TurnHandlerOutcome::Break(TurnLoopResult::Completed { .. })));
        assert!(!ctx.is_recovery_active());
    }

    #[tokio::test]
    async fn low_signal_search_churn_schedules_recovery_and_progress_only_recovery_text_completes() {
        let mut backing = TestTurnProcessingBacking::new(8).await;
        backing.set_loop_limit(tool_names::CODE_SEARCH, 2);
        let seeded_args = json!({"query":"Result","path":"src"});
        assert!(backing.record_tool_call(tool_names::CODE_SEARCH, &seeded_args).is_none());
        let _ = backing.record_tool_call(tool_names::CODE_SEARCH, &seeded_args);
        let warning = backing.record_tool_call(tool_names::CODE_SEARCH, &seeded_args);
        assert!(warning.is_some());
        assert!(backing.is_hard_limit_exceeded(tool_names::CODE_SEARCH));
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({"results": []}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        let same_args = json!({"query":"Result","path":"src"});
        update_repetition_tracker(&mut tracker, &miss, tool_names::CODE_SEARCH, &same_args);
        update_repetition_tracker(&mut tracker, &miss, tool_names::CODE_SEARCH, &same_args);
        update_repetition_tracker(&mut tracker, &miss, tool_names::CODE_SEARCH, &same_args);

        let balancer_outcome = super::handle_turn_balancer(&mut ctx, 4, &mut tracker, 4, 3).await;
        assert!(matches!(balancer_outcome, TurnHandlerOutcome::Continue));
        assert!(ctx.is_recovery_active());
        assert!(ctx.consume_recovery_pass());

        let recovery_outcome = ctx
            .handle_text_response("Let me try a narrower search next.".to_string(), Vec::new(), None, None, false)
            .await
            .expect("recovery response should be handled");

        assert!(matches!(recovery_outcome, TurnHandlerOutcome::Break(TurnLoopResult::Completed { .. })));
        assert!(!ctx.is_recovery_active());
        assert!(backing.is_hard_limit_exceeded(tool_names::CODE_SEARCH));
    }

    #[tokio::test]
    async fn early_low_signal_search_churn_schedules_recovery_before_turn_window() {
        let mut backing = TestTurnProcessingBacking::new(20).await;
        let mut ctx = backing.turn_processing_context();
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({"results": []}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        let same_args = json!({"query":"Result","path":"src"});
        update_repetition_tracker(&mut tracker, &miss, tool_names::CODE_SEARCH, &same_args);
        update_repetition_tracker(&mut tracker, &miss, tool_names::CODE_SEARCH, &same_args);
        update_repetition_tracker(&mut tracker, &miss, tool_names::CODE_SEARCH, &same_args);

        let balancer_outcome = super::handle_turn_balancer(&mut ctx, 3, &mut tracker, 20, 3).await;
        assert!(matches!(balancer_outcome, TurnHandlerOutcome::Continue));
        assert!(ctx.is_recovery_active());
        assert_eq!(tracker.consecutive_navigations, 0);
        assert!(ctx.working_history.iter().any(|message| {
            message
                .content
                .as_text()
                .contains("Repeated low-signal navigation calls reached the per-turn fast-path cap")
        }));
    }
}
