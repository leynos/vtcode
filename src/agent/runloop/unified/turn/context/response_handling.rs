use super::*;
use crate::agent::runloop::unified::plan_blocks::strip_plan_persistence_policy_line;
use crate::agent::runloop::unified::planning_workflow::{
    PlanArtefactError, ValidatedPlanArtefact, emit_plan_ready_events, persist_plan_draft, persisted_plan_is_ready,
    plan_repair_directive_for_error, validate_plan_content,
};
use crate::agent::runloop::unified::turn::turn_processing::resolve_effective_request_model;
use crate::agent::runloop::unified::ui_interaction_stream_helpers::render_compact_reasoning_block;

const DENIED_INTERVIEW_PLAN_SYNTHESIS_RETRY_DIRECTIVE: &str = "Planning recovery: the interactive interview is unavailable, and the previous response did not contain a completed plan. Do not ask another question or offer approval yet. Emit exactly one compact `<proposed_plan>` now from the repository evidence already in this conversation; include Summary, numbered steps in the form `Action -> files: [path] -> verify: [command]`, Validation, and short Assumptions. Do not emit tool calls.";

const PLAN_PSEUDO_TOOL_CALL_REPROMPT_DIRECTIVE: &str = "Planning: the previous response contained tool-call markup that was not executed — XML tool-call text is not a tool call. If you need more repository evidence, invoke tools through the tool-call channel now. Otherwise present the completed plan as one compact `<proposed_plan>` (Summary, numbered steps in the form `Action -> files: [path] -> verify: [command]`, Validation, short Assumptions). Do not emit XML tool-call markup as text.";

/// Detect whether a planning-mode text response is a clarifying question
/// posed to the user rather than a plan or research prose. The deterministic
/// interview-denial recovery must NOT force plan synthesis when the model is
/// legitimately asking the user a question in plain text (the text-mode
/// equivalent of the unavailable `request_user_input` modal). Without this
/// check, the retry directive suppresses the question and the agent proceeds
/// to propose a plan without waiting for the user's answer (checkpoint
/// turn_856).
///
/// Heuristic: the last non-empty line ends with `?`. This is a strong signal
/// that the model is asking a question, and it does not match completed plans
/// (which end with Assumptions/Validation prose) or research dumps.
pub(super) fn looks_like_clarifying_question(text: &str) -> bool {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|last_line| last_line.trim().ends_with('?'))
}

impl<'a> TurnProcessingContext<'a> {
    /// End a failed plan-mode recovery pass with an explicit resumable
    /// handoff. This path intentionally emits a `Blocked` outcome: no plan
    /// was approved, but the planning session and its bounded evidence remain
    /// available for a later `keep planning`/restatement turn.
    pub(crate) fn break_planning_recovery_with_handoff(
        &mut self,
        detail: &str,
        rejected_plan: Option<&str>,
    ) -> anyhow::Result<TurnHandlerOutcome> {
        let detail = detail.trim();
        let detail = if detail.is_empty() {
            "the synthesis pass failed"
        } else {
            detail
        };
        let detail = if detail.len() > 240 {
            let mut end = 240;
            while end > 0 && !detail.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &detail[..end])
        } else {
            detail.to_string()
        };
        let message = format!(
            "Planning remains active, but the one tool-free recovery synthesis did not produce an approval-ready plan ({detail}). The latest request and bounded evidence are preserved. Re-state the planning request or type `keep planning` to try again; no changes were applied."
        );

        self.harness_state.mark_final_response_fallback();
        self.handle_assistant_response(message, Vec::new(), None, false, Some(uni::AssistantPhase::FinalAnswer))?;
        if let Some(rejected_plan) = rejected_plan {
            append_rejected_plan_draft_to_last_assistant(self.working_history, rejected_plan);
        }
        self.finish_recovery_pass();
        Ok(TurnHandlerOutcome::Break(TurnLoopResult::Blocked {
            reason: Some(
                "planning recovery did not produce an approval-ready plan; planning remains active".to_string(),
            ),
        }))
    }

    fn reject_plan_artefact(
        &mut self,
        error: PlanArtefactError,
        plan_text: &str,
        allow_repair: bool,
    ) -> anyhow::Result<TurnHandlerOutcome> {
        use vtcode_core::utils::ansi::MessageStyle;

        if self.recovery_is_tool_free() && self.is_planning_active() {
            return self.break_planning_recovery_with_handoff(
                &format!("the synthesized draft failed validation: {error}"),
                Some(plan_text),
            );
        }

        if allow_repair && self.plan_session.plan_validation_repair_allowed() {
            self.plan_session.mark_plan_validation_repair_used();
            // The error→feedback mapping and bounded repair policy live in the
            // planning facade so this initial-plan rejection path and the
            // later-turn approval rejection path share identical guidance.
            tracing::warn!(
                target: "vtcode.planning_workflow",
                error = %error,
                repair_scheduled = true,
                "plan artefact rejected before approval; scheduling bounded repair"
            );
            // Keep the rejected draft in assistant history so the repair
            // request can inspect it without elevating model- or
            // repository-controlled text into a system message.
            append_rejected_plan_draft_to_last_assistant(self.working_history, plan_text);
            let directive = plan_repair_directive_for_error(&error);
            self.push_system_message(directive);
            return Ok(TurnHandlerOutcome::Continue);
        }
        let message = format!("Plan is not ready for approval: {error}");
        self.renderer.line(MessageStyle::Warning, &message)?;
        tracing::warn!(target: "vtcode.planning_workflow", error = %error, "plan artefact rejected before approval");
        append_rejected_plan_draft_to_last_assistant(self.working_history, plan_text);
        // Terminal rejection: the draft is never persisted or shown by the
        // approval flow, so render it here — otherwise the user cannot see
        // what was rejected or revise it manually (checkpoint turn_912).
        if !plan_text.trim().is_empty() {
            self.renderer.line(MessageStyle::Info, "Rejected plan draft:")?;
            self.renderer.line(MessageStyle::Response, plan_text)?;
        }
        self.renderer.line(
            MessageStyle::Warning,
            crate::agent::runloop::unified::planning_workflow_state::PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT,
        )?;
        Ok(TurnHandlerOutcome::Break(TurnLoopResult::Completed { plan_approved_execution_pending: false }))
    }

    /// Schedule the one bounded plan-only retry allowed after a permanent
    /// interview denial. Keeping the transition here prevents callers from
    /// duplicating the denial/recovery state machine.
    pub(crate) fn retry_denied_interview_plan_synthesis(&mut self) -> bool {
        if !self.is_planning_active() || !self.plan_session.plan_synthesis_retry_allowed() {
            return false;
        }

        self.plan_session.mark_plan_synthesis_retry_used();
        self.push_system_message(DENIED_INTERVIEW_PLAN_SYNTHESIS_RETRY_DIRECTIVE);
        self.harness_state.retry_recovery_pass()
    }

    pub(crate) fn handle_assistant_response(
        &mut self,
        text: String,
        reasoning: Vec<ReasoningSegment>,
        reasoning_details: Option<Vec<String>>,
        response_streamed: bool,
        phase: Option<uni::AssistantPhase>,
    ) -> anyhow::Result<()> {
        let mut text = text;
        let detail_reasoning = reasoning_details
            .as_deref()
            .and_then(vtcode_core::llm::providers::common::extract_reasoning_text_from_serialized_details);
        if should_suppress_redundant_diff_recap(self.working_history, &text) {
            text.clear();
        }
        let has_visible_text = !text.trim().is_empty();
        let final_response_text = matches!(phase, Some(uni::AssistantPhase::FinalAnswer))
            .then(|| text.clone())
            .filter(|text| !text.trim().is_empty());
        if !reasoning.is_empty() || reasoning_details.as_ref().is_some_and(|details| !details.is_empty()) {
            tracing::info!(
                target: "vtcode.turn.metrics",
                metric = "reasoning_observed",
                run_id = %self.harness_state.run_id.0,
                turn_id = %self.harness_state.turn_id.0,
                phase = match phase {
                    Some(uni::AssistantPhase::Commentary) => "commentary",
                    Some(uni::AssistantPhase::FinalAnswer) => "final_answer",
                    None => "unspecified",
                },
                reasoning_segments = reasoning.len(),
                reasoning_details = reasoning_details.as_ref().map_or(0, Vec::len),
                has_detail_reasoning = detail_reasoning.is_some(),
                has_visible_text,
                response_streamed,
                "turn metric"
            );
        }

        if !response_streamed {
            use vtcode_core::utils::ansi::MessageStyle;

            if !text.trim().is_empty() {
                self.renderer.line(MessageStyle::Response, &text)?;
            }
            let mut rendered_reasoning = detail_reasoning.is_some().then(|| Vec::with_capacity(reasoning.len()));

            for segment in &reasoning {
                if let Some(stage) = &segment.stage {
                    self.handle.set_reasoning_stage(Some(stage.clone()));
                }

                let reasoning_text = &segment.text;
                if !reasoning_text.trim().is_empty() {
                    let duplicates_content = has_visible_text && reasoning_duplicates_content(reasoning_text, &text);
                    if !duplicates_content {
                        let compact = vtcode_commons::formatting::compact_reasoning_text(reasoning_text);
                        if compact.trim().is_empty() {
                            continue;
                        }
                        let rendered = render_compact_reasoning_block(self.renderer, reasoning_text)?;
                        if rendered && let Some(rendered_reasoning) = rendered_reasoning.as_mut() {
                            rendered_reasoning.push(compact);
                        }
                    }
                }
            }

            if let Some(detail_text) = detail_reasoning.as_deref() {
                let cleaned_detail = vtcode_commons::formatting::compact_reasoning_text(detail_text);
                let duplicates_content = has_visible_text && reasoning_duplicates_content(&cleaned_detail, &text);
                let duplicates_rendered = rendered_reasoning.as_ref().is_some_and(|rendered_reasoning| {
                    rendered_reasoning.iter().any(|existing: &String| {
                        reasoning_duplicates_content(existing, &cleaned_detail)
                            || reasoning_duplicates_content(&cleaned_detail, existing)
                    })
                });
                if !cleaned_detail.is_empty() && !duplicates_content && !duplicates_rendered {
                    render_compact_reasoning_block(self.renderer, detail_text)?;
                }
            }
            self.handle.set_reasoning_stage(None);
        }

        let combined_reasoning = build_combined_reasoning(&reasoning, detail_reasoning.as_deref());
        let include_reasoning = combined_reasoning
            .as_deref()
            .is_some_and(|combined_reasoning| !reasoning_duplicates_content(combined_reasoning, &text));
        let msg = uni::Message::assistant(text).with_phase(phase);
        let mut msg_with_reasoning = if include_reasoning {
            msg.with_reasoning(combined_reasoning)
        } else {
            msg
        };

        if let Some(details) = reasoning_details.filter(|d| !d.is_empty()) {
            let payload = details
                .into_iter()
                .map(|detail| parse_reasoning_detail_value(&detail))
                .collect::<Vec<_>>();
            msg_with_reasoning = msg_with_reasoning.with_reasoning_details(Some(payload));
        }

        if !msg_with_reasoning.content.as_text().is_empty()
            || msg_with_reasoning.reasoning.is_some()
            || msg_with_reasoning.reasoning_details.is_some()
        {
            push_assistant_message(self.working_history, msg_with_reasoning);
        }

        if let Some(final_response_text) = final_response_text {
            self.harness_state.mark_final_response_rendered();
            if self.harness_emitter.is_none() || self.harness_state.streamed_response_event_emitted() {
                self.harness_state.mark_final_response_event_emitted();
            } else if !self.harness_state.final_response_event_emitted()
                && let Some(emitter) = self.harness_emitter
            {
                match emitter.emit_assistant_message(&self.harness_state.turn_id.0, &final_response_text) {
                    Ok(()) => self.harness_state.mark_final_response_event_emitted(),
                    Err(err) => tracing::warn!(error = %err, "final assistant message harness emission failed"),
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn handle_text_response(
        &mut self,
        text: String,
        reasoning: Vec<ReasoningSegment>,
        reasoning_details: Option<Vec<String>>,
        proposed_plan: Option<String>,
        response_streamed: bool,
    ) -> anyhow::Result<TurnHandlerOutcome> {
        let recovery_pass_response = self.is_recovery_active() && self.recovery_pass_used();
        let tool_free_recovery_pass = recovery_pass_response && self.recovery_is_tool_free();
        // Tool-free recovery is terminal: the model's text IS the final answer.
        // Some providers (e.g. MiniMax) emit a noise prefix like `]<]minimax[>[`
        // before/instead of real content. When the model has nothing to
        // synthesize, this residue becomes the user-visible final answer — the
        // "agent just stops with garbage" symptom (checkpoints turn_609/613).
        // Strip known noise and, if nothing meaningful remains, substitute a
        // clear fallback so the user gets an actionable message instead of
        // provider noise.
        // Strip provider noise (e.g. MiniMax `]<]minimax[>[`) from ALL assistant
        // text — commentary, normal final answers, and recovery final answers.
        // This prevents noise from leaking into the user-visible output and,
        // more importantly, from being echoed back to the API via
        // `working_history` on follow-up calls (polluted context degrades
        // subsequent responses and contributes to post-tool follow-up
        // failures). For tool-free recovery passes, additionally substitute a
        // fallback when nothing meaningful remains after stripping.
        let text = if tool_free_recovery_pass {
            crate::agent::runloop::unified::turn::provider_noise::sanitize_recovery_answer(text)
        } else {
            crate::agent::runloop::unified::turn::provider_noise::strip_provider_noise(&text)
        };
        let text = if proposed_plan.is_some() {
            strip_plan_persistence_policy_line(&text)
        } else {
            text
        };
        // Plan-mode salvage: a model with no tool schemas on the wire (or a
        // confused checkpoint) sometimes answers with XML-ish tool-call markup
        // as text. No textual parser could execute it, and in plan mode any
        // text ends the turn, so the raw markup became the user-visible final
        // answer and leaked into history, ATIF, and harness logs
        // (turn_887/turn_888). Strip the markup from the stored/visible text;
        // a bounded re-prompt below gives the model a chance to call tools
        // natively or present the plan instead.
        let pseudo_tool_call_markup_detected = self.is_planning_active()
            && !tool_free_recovery_pass
            && proposed_plan.is_none()
            && crate::agent::runloop::text_tools::contains_pseudo_tool_call_markers(&text);
        let text = if pseudo_tool_call_markup_detected {
            crate::agent::runloop::text_tools::strip_textual_tool_call_regions(&text)
                .trim()
                .to_string()
        } else {
            text
        };
        if tool_free_recovery_pass && self.is_planning_active() {
            if proposed_plan.is_none() {
                let rejected_plan = text
                    .contains("<proposed_plan>")
                    .then_some(text.as_str())
                    .or_else(|| text.contains("<plan>").then_some(text.as_str()));
                return self.break_planning_recovery_with_handoff(
                    "the response did not contain exactly one completed <proposed_plan> block",
                    rejected_plan,
                );
            }
            if !text.trim().is_empty() {
                return self.break_planning_recovery_with_handoff(
                    "the response included prose outside the required <proposed_plan> block",
                    proposed_plan.as_deref(),
                );
            }
        }
        let denied_interview_plan_retry = self.is_planning_active()
            && !tool_free_recovery_pass
            && proposed_plan.is_none()
            && !text.trim().is_empty()
            && self.plan_session.plan_synthesis_retry_allowed();
        let denied_interview_recovery_retry = self.is_planning_active()
            && tool_free_recovery_pass
            && proposed_plan.is_none()
            && self.plan_session.plan_synthesis_retry_allowed();
        let denied_interview_without_ready_plan = self.is_planning_active()
            && !tool_free_recovery_pass
            && self.plan_session.is_interview_denied()
            && proposed_plan.is_none()
            && !persisted_plan_is_ready(&self.tool_registry.planning_workflow_state()).await
            && !looks_like_clarifying_question(&text);
        let text = if denied_interview_without_ready_plan {
            crate::agent::runloop::unified::planning_workflow_state::PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT
                .to_string()
        } else {
            text
        };
        // Dead-end guard (checkpoint turn_912): decide whether this planning
        // response qualifies for the no-approval-ready hint, but DEFER the
        // append until terminality is known — the pseudo-tool-call reprompt
        // and stop-hook paths below can still return `Continue`, and storing
        // the hint before a continuation would leave stale "no approval-ready
        // plan" guidance in history for a turn that kept going. Clarifying
        // questions are excluded — the turn ends intentionally for the user's
        // answer — and the denied-interview path above already surfaced the
        // hint as the response text. Responses carrying a `proposed_plan` are
        // also excluded: the plan-validation/persistence branch owns their
        // terminal messaging.
        let defer_no_ready_plan_hint = self.is_planning_active()
            && !tool_free_recovery_pass
            && proposed_plan.is_none()
            && should_render_no_ready_plan_hint(denied_interview_without_ready_plan, &text)
            && !persisted_plan_is_ready(&self.tool_registry.planning_workflow_state()).await;
        let final_text = text.clone();
        let consecutive_relaxed = self.harness_state.consecutive_relaxed_continuations;
        let continuation_decision = if tool_free_recovery_pass {
            // Tool-free recovery is terminal: the text produced during recovery
            // IS the final answer. Allowing continuation here would call
            // `finish_recovery_pass()` (deactivating recovery), re-enable tools
            // on the next iteration, and — if the follow-up fails again —
            // re-trigger recovery, producing an infinite cycle that no existing
            // bound catches (`consecutive_relaxed_continuations` is bypassed by
            // non-relaxed "recent_tool_activity" continuations that reset the
            // counter to 0, and `MAX_RECOVERY_RETRIES` only counts retries
            // within a single pass). Evaluate continuation intent solely to
            // populate diagnostic fields for the tracing log; the decision is
            // always to end the turn.
            let decision = evaluate_interim_text_continuation(
                self.full_auto,
                self.is_planning_active(),
                self.working_history,
                &text,
                consecutive_relaxed,
            );
            InterimTextContinuationDecision {
                should_continue: false,
                reason: "tool_free_recovery_terminal",
                is_interim_progress: decision.is_interim_progress,
                last_user_follow_up: decision.last_user_follow_up,
                recent_tool_activity: decision.recent_tool_activity,
                last_user_requested_progressive_work: decision.last_user_requested_progressive_work,
                is_relaxed_continuation: false,
            }
        } else {
            evaluate_interim_text_continuation(
                self.full_auto,
                self.is_planning_active(),
                self.working_history,
                &text,
                consecutive_relaxed,
            )
        };

        // Track consecutive relaxed continuations to prevent infinite loops.
        if continuation_decision.should_continue && continuation_decision.is_relaxed_continuation {
            self.harness_state.consecutive_relaxed_continuations += 1;
        } else if continuation_decision.should_continue {
            // Non-relaxed continuation resets the counter
            self.harness_state.consecutive_relaxed_continuations = 0;
        } else {
            // Turn is ending, reset the counter
            self.harness_state.consecutive_relaxed_continuations = 0;
        }

        let assistant_phase = if continuation_decision.should_continue {
            Some(uni::AssistantPhase::Commentary)
        } else {
            Some(uni::AssistantPhase::FinalAnswer)
        };
        self.handle_assistant_response(text, reasoning, reasoning_details, response_streamed, assistant_phase)?;

        // Count this text response so the recovery loop can short-circuit
        // when the model has already produced a final answer but the loop
        // keeps re-prompting. See `MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN`.
        self.harness_state.record_assistant_text_response();

        if recovery_pass_response {
            self.finish_recovery_pass();
        }

        // A tool-free pass is normally terminal, but a permanently denied
        // interview has one additional bounded contract: it must produce a
        // real draft before the user can approve anything. If the provider
        // ignored the recovery directive and returned prose without a plan,
        // retry once while tools remain disabled instead of ending mid-turn
        // with no approval-ready draft.
        //
        // EXCEPTION: if the text is a clarifying question (the text-mode
        // equivalent of the unavailable interview modal), end the turn so the
        // user can answer it. Forcing plan synthesis here would suppress the
        // question and proceed to propose a plan without user input
        // (checkpoint turn_856).
        if denied_interview_recovery_retry {
            if looks_like_clarifying_question(&final_text) {
                tracing::info!(
                    target: "vtcode.planning_workflow",
                    "denied interview recovery produced a clarifying question; ending turn for user input instead of retrying plan synthesis"
                );
                // Fall through to normal turn completion — the question is
                // already in working_history as the assistant's final answer.
            } else if self.retry_denied_interview_plan_synthesis() {
                tracing::info!(
                    target: "vtcode.planning_workflow",
                    "retrying tool-free synthesis after denied interview returned no plan"
                );
                return Ok(TurnHandlerOutcome::Continue);
            }
        }

        // A permanent interview denial is different from a cancelled
        // interview: the model must still produce a real draft before the
        // user can approve it. The denial diagnostic is advisory, so some
        // models answer only with "type yes" instead of emitting a plan.
        // Give that response one bounded synthesis retry. This keeps the
        // approval path draft-backed without re-enabling the unavailable
        // interview tool or allowing an unbounded continuation loop.
        //
        // EXCEPTION: a clarifying question is the text-mode equivalent of
        // the unavailable interview modal — end the turn for user input
        // instead of suppressing it with a forced synthesis retry.
        if denied_interview_plan_retry && !looks_like_clarifying_question(&final_text) {
            self.plan_session.mark_plan_synthesis_retry_used();
            self.push_system_message(DENIED_INTERVIEW_PLAN_SYNTHESIS_RETRY_DIRECTIVE);
            tracing::info!(
                target: "vtcode.planning_workflow",
                "retrying denied interview response as a bounded plan synthesis"
            );
            return Ok(TurnHandlerOutcome::Continue);
        }

        // Plan-mode pseudo-tool-call reprompt: a model with no tool schemas on
        // the wire (or a confused checkpoint) sometimes emits XML-ish
        // tool-call markup as text. No textual parser could execute it, and in
        // plan mode any text ends the turn, so the raw markup previously
        // became the user-visible final answer and leaked into history, ATIF,
        // and harness logs (turn_887/turn_888). The markup was already stripped
        // from the stored text above; give the model a bounded chance to call
        // tools natively or present the plan instead of ending mid-turn with
        // cleaned-up prose. When the reprompt budget is exhausted, fall through
        // to normal turn completion — the already-stripped text guarantees raw
        // markup never reaches the user.
        if pseudo_tool_call_markup_detected && self.plan_session.plan_pseudo_tool_call_reprompt_allowed() {
            self.plan_session.mark_plan_pseudo_tool_call_reprompt_used();
            self.push_system_message(PLAN_PSEUDO_TOOL_CALL_REPROMPT_DIRECTIVE);
            tracing::info!(
                target: "vtcode.planning_workflow",
                "re-prompting after pseudo-tool-call markup in plan mode"
            );
            return Ok(TurnHandlerOutcome::Continue);
        }

        tracing::info!(
            target: "vtcode.turn.metrics",
            metric = "text_response_decision",
            run_id = %self.harness_state.run_id.0,
            turn_id = %self.harness_state.turn_id.0,
            should_continue = continuation_decision.should_continue,
            reason = continuation_decision.reason,
            is_interim_progress = continuation_decision.is_interim_progress,
            last_user_follow_up = continuation_decision.last_user_follow_up,
            recent_tool_activity = continuation_decision.recent_tool_activity,
            last_user_requested_progressive_work =
                continuation_decision.last_user_requested_progressive_work,
            recovery_pass_response,
            tool_free_recovery_pass,
            planning_workflow = self.is_planning_active(),
            full_auto = self.full_auto,
            history_len = self.working_history.len(),
            "turn metric"
        );

        if continuation_decision.should_continue {
            push_system_directive_once(self.working_history, AUTONOMOUS_CONTINUE_DIRECTIVE);
            return Ok(TurnHandlerOutcome::Continue);
        }

        if let Some(hooks) = self.lifecycle_hooks {
            let outcome = hooks.run_stop(&final_text, self.harness_state.stop_hook_active).await?;
            crate::agent::runloop::unified::turn::utils::render_hook_messages(self.renderer, &outcome.messages)?;
            if let Some(reason) = outcome.block_reason {
                push_system_directive_once(self.working_history, &reason);
                self.harness_state.stop_hook_active = true;
                return Ok(TurnHandlerOutcome::Continue);
            }
        }
        self.harness_state.stop_hook_active = false;

        // Terminal planless exit: every continuation path (interim-progress,
        // denied-interview retry, pseudo-tool-call reprompt, stop-hook) has
        // returned above, so the turn is ending. Now surface the deferred
        // no-approval-ready hint — rendered for the user AND appended to the
        // stored assistant message so it survives in history, ATIF, and
        // harness logs. A turn that continued never stores the hint.
        if defer_no_ready_plan_hint {
            use vtcode_core::utils::ansi::MessageStyle;
            let hint =
                crate::agent::runloop::unified::planning_workflow_state::PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT;
            self.renderer.line(MessageStyle::Info, hint)?;
            append_no_ready_plan_hint_to_last_assistant(self.working_history);
        }

        if let Some(plan_text) = proposed_plan {
            let planning_active = self.is_planning_active();
            tracing::info!(
                target: "vtcode.planning_workflow",
                plan_ready = true,
                planning_active,
                "completed plan reached approval handoff"
            );
            // Persist before publishing the approval request so consumers that
            // follow the event's plan_file can read the completed draft.
            let validation = validate_plan_content(&plan_text);
            if !validation.is_ready() {
                let error = PlanArtefactError::Invalid {
                    reasons: validation.reasons().join("; "),
                    report: Box::new(validation),
                };
                return self.reject_plan_artefact(error, &plan_text, !tool_free_recovery_pass);
            }

            let persisted = match persist_plan_draft(&self.tool_registry.planning_workflow_state(), &plan_text).await {
                Ok(persisted) => persisted,
                Err(error) => {
                    let error = PlanArtefactError::Persistence { reason: error.to_string() };
                    return self.reject_plan_artefact(error, &plan_text, false);
                }
            };
            // `persist_plan_draft` already validated the same immutable text
            // before writing, so re-checking `persisted.validation.is_ready()`
            // here is redundant. The persisted-readiness gate below rereads the
            // file from disk and verifies sidecar trackers exist — that check
            // is NOT redundant and stays.
            if !persisted_plan_is_ready(&self.tool_registry.planning_workflow_state()).await {
                let error = PlanArtefactError::Persistence {
                    reason: "plan, sidecar tracker, and workspace tracker were not published completely".to_string(),
                };
                return self.reject_plan_artefact(error, &plan_text, false);
            }
            // Construct from the already-validated report instead of
            // re-parsing the same immutable text a fourth time.
            let plan = ValidatedPlanArtefact::from_validated(
                persisted.plan_file.clone(),
                plan_text.clone(),
                persisted.validation.clone(),
            );
            let plan_state = self.tool_registry.planning_workflow_state();
            emit_plan_ready_events(
                self.plan_session,
                &plan_state,
                self.harness_emitter,
                &self.harness_state.run_id.0,
                &self.harness_state.turn_id.0,
                &plan_text,
            )
            .await;

            let require_confirmation = self.vt_cfg.map(|cfg| cfg.agent.require_plan_confirmation).unwrap_or(true);
            let supports_inline = self.renderer.supports_inline_ui();
            tracing::info!(
                target: "vtcode.planning_workflow",
                plan_ready = true,
                require_confirmation,
                supports_inline_ui = supports_inline,
                "plan approval overlay condition check"
            );
            let approval_route = crate::agent::runloop::unified::planning_workflow::plan_approval_route(
                require_confirmation,
                supports_inline,
                self.skip_confirmations,
                self.full_auto,
            );
            tracing::info!(
                target: "vtcode.planning_workflow",
                ?approval_route,
                "plan approval route selected"
            );
            if approval_route == crate::agent::runloop::unified::planning_workflow::PlanApprovalRoute::Inline {
                use crate::agent::runloop::unified::planning_workflow::{
                    PlanApprovalRequestContext, PlanApprovalTelemetryContext, execute_plan_approval,
                };
                return execute_plan_approval(
                    self.tool_registry,
                    self.plan_session,
                    self.handle,
                    self.session,
                    self.ctrl_c_state,
                    self.ctrl_c_notify,
                    PlanApprovalRequestContext {
                        plan: &plan,
                        active_agent_name: self.active_primary_agent.active().name(),
                        skip_confirmations: self.skip_confirmations,
                        context_usage_percent: self.context_manager.context_usage_percent(
                            vtcode_core::compaction::effective_context_budget(
                                self.vt_cfg,
                                &**self.provider_client,
                                &resolve_effective_request_model(
                                    &self.config.model,
                                    self.active_primary_agent.active(),
                                ),
                            ),
                        ),
                    },
                    PlanApprovalTelemetryContext {
                        emitter: self.harness_emitter,
                        thread_id: &self.harness_state.run_id.0,
                        turn_id: &self.harness_state.turn_id.0,
                    },
                )
                .await;
            }

            use vtcode_core::utils::ansi::MessageStyle;
            self.renderer.line(MessageStyle::Info, "Plan ready for approval:")?;
            self.renderer.line(MessageStyle::Response, &plan_text)?;
            if approval_route == crate::agent::runloop::unified::planning_workflow::PlanApprovalRoute::Headless {
                self.renderer.line(
                    MessageStyle::Info,
                    "Plan is awaiting approval. Type `approve`, `implement`, or `yes` to begin execution, or `edit` to revise the plan.",
                )?;
                return Ok(TurnHandlerOutcome::Break(TurnLoopResult::Completed {
                    plan_approved_execution_pending: false,
                }));
            }

            self.renderer
                .line(MessageStyle::Info, "Plan approved by the active execution policy; starting implementation.")?;
            let handoff = crate::agent::runloop::unified::planning_workflow::complete_approved_plan_handoff(
                self.tool_registry,
                self.plan_session,
                self.handle,
                plan,
                self.active_primary_agent.active().name(),
                true,
                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
            )
            .await;
            let handoff = match handoff {
                Ok(handoff) => handoff,
                Err(error) => {
                    tracing::warn!(target: "vtcode.planning_workflow", error = %error, "automatic approved-plan handoff blocked");
                    crate::agent::runloop::unified::planning_workflow::resolve_plan_approval(
                        self.plan_session,
                        self.harness_emitter,
                        &self.harness_state.run_id.0,
                        &self.harness_state.turn_id.0,
                        vtcode_core::exec::events::PlanApprovalDecision::Cancel,
                        true,
                    );
                    let message = format!("Plan execution is blocked: {error}");
                    self.renderer.line(MessageStyle::Error, &message)?;
                    return Ok(TurnHandlerOutcome::Break(TurnLoopResult::Completed {
                        plan_approved_execution_pending: false,
                    }));
                }
            };
            crate::agent::runloop::unified::planning_workflow::resolve_plan_approval(
                self.plan_session,
                self.harness_emitter,
                &self.harness_state.run_id.0,
                &self.harness_state.turn_id.0,
                vtcode_core::exec::events::PlanApprovalDecision::AutoAccept,
                true,
            );
            let execution_agent = handoff.execution_agent;
            let handoff_skip_confirmations = handoff.skip_confirmations;
            if let Some(agent) = execution_agent {
                return Ok(TurnHandlerOutcome::SwitchPrimaryAgentWithPolicy {
                    agent,
                    skip_confirmations: handoff_skip_confirmations,
                    execution_context: crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
                });
            }
            return Ok(TurnHandlerOutcome::BreakWithPolicy {
                result: TurnLoopResult::Completed { plan_approved_execution_pending: true },
                skip_confirmations: handoff_skip_confirmations,
                execution_context: crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
            });
        }

        Ok(TurnHandlerOutcome::Break(TurnLoopResult::Completed { plan_approved_execution_pending: false }))
    }
}

/// Pure predicate for the planless-planning-turn hint; kept separate from the
/// async persistence check so the decision is unit-testable.
pub(super) fn should_render_no_ready_plan_hint(denied_interview_hint_shown: bool, final_text: &str) -> bool {
    !denied_interview_hint_shown && !looks_like_clarifying_question(final_text)
}

/// Append the no-approval-ready hint to the stored assistant message from
/// this response, so the guidance survives in history, ATIF, and harness
/// logs. The message was pushed by `handle_assistant_response` before
/// terminality was known; mutating the trailing assistant message keeps the
/// hint attached to the answer it explains. Content is rewritten through
/// `MessageContent::text`, which is lossless here: the message just pushed
/// above is always built from a plain text string.
fn append_no_ready_plan_hint_to_last_assistant(working_history: &mut [uni::Message]) {
    append_to_last_assistant_message(
        working_history,
        crate::agent::runloop::unified::planning_workflow_state::PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT,
    );
}

/// Cap for a rejected plan draft re-attached to the assistant message that
/// produced it. Valid plans are compact by contract (<4KB), so 8KB bounds
/// pathological drafts while never truncating a legitimate one.
const REJECTED_PLAN_DRAFT_HISTORY_BUDGET: usize = 8 * 1024;

/// Re-attach a rejected `<proposed_plan>` draft to the last assistant
/// message. The plan block is extracted from the response text before
/// `handle_assistant_response` stores it, so without this the draft vanishes
/// from history on rejection: the repair retry cannot see what it is fixing,
/// and terminal rejections leave checkpoints/events with no trace of the
/// rejected plan (turn_912/913: the final assistant message degraded to the
/// planning-workflow reminder bullet). Keeping it as assistant content also
/// prevents untrusted draft text from being interpreted as system guidance.
fn append_rejected_plan_draft_to_last_assistant(working_history: &mut [uni::Message], plan_text: &str) {
    let Some(draft) = bounded_rejected_plan_draft(plan_text) else {
        return;
    };
    append_to_last_assistant_message(working_history, &draft);
}

fn bounded_rejected_plan_draft(plan_text: &str) -> Option<String> {
    let plan_text = plan_text.trim();
    if plan_text.is_empty() {
        return None;
    }
    let bounded = if plan_text.len() > REJECTED_PLAN_DRAFT_HISTORY_BUDGET {
        let mut end = REJECTED_PLAN_DRAFT_HISTORY_BUDGET;
        while !plan_text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…[truncated]", &plan_text[..end])
    } else {
        plan_text.to_string()
    };
    Some(format!("<proposed_plan>\n{bounded}\n</proposed_plan>"))
}

fn append_to_last_assistant_message(working_history: &mut [uni::Message], addition: &str) {
    let Some(last) = working_history
        .iter_mut()
        .rev()
        .find(|message| message.role == uni::MessageRole::Assistant)
    else {
        return;
    };
    let text = last.content.as_text();
    let updated = if text.trim().is_empty() {
        addition.to_string()
    } else {
        format!("{}\n\n{}", text.trim_end(), addition)
    };
    last.content = uni::MessageContent::text(updated);
}

// NOTE: Provider-noise stripping (MiniMax `]<]minimax[>[` and similar) has been
// centralized in `turn::provider_noise`. All call sites — textual tool parsers,
// response handling, and the live stream renderer — delegate to
// `strip_provider_noise` / `sanitize_recovery_answer` there. See that module
// for the canonical noise vocabulary and comprehensive tests.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::runloop::unified::turn::turn_processing::test_support::TestTurnProcessingBacking;

    /// Unparseable pseudo-tool-call markup: the `<tools:call>` name is empty,
    /// so every textual parser rejects it, but the pseudo-marker scan still
    /// sees `<tool_call`. Mirrors the raw-XML leak from turn_887/turn_888.
    const BROKEN_MARKUP_RESPONSE: &str =
        "I need to inspect the workspace.\n<tool_call>\n<tools:call name=\"\">\n</tools:call>\n</tool_call>";

    #[tokio::test]
    async fn plan_mode_pseudo_tool_call_markup_is_stripped_and_reprompts_once() {
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();

        let outcome = ctx
            .handle_text_response(BROKEN_MARKUP_RESPONSE.to_string(), Vec::new(), None, None, false)
            .await
            .expect("text response should be handled");

        assert!(
            matches!(outcome, TurnHandlerOutcome::Continue),
            "plan-mode pseudo-tool-call markup should re-prompt instead of ending the turn"
        );

        let assistant_texts: Vec<String> = ctx
            .working_history
            .iter()
            .filter(|message| message.role == uni::MessageRole::Assistant)
            .map(|message| message.content.as_text().into_owned())
            .collect();
        assert!(
            assistant_texts
                .iter()
                .any(|text| text.contains("I need to inspect the workspace.")),
            "the prose part of the response should be preserved: {assistant_texts:?}"
        );
        assert!(
            assistant_texts.iter().all(|text| !text.contains("<tool_call")),
            "raw tool-call markup must never be stored in history: {assistant_texts:?}"
        );

        let directive_present = ctx.working_history.iter().any(|message| {
            message.role == uni::MessageRole::System && message.content.as_text().contains("not executed")
        });
        assert!(directive_present, "a re-prompt directive should be pushed into history");
    }

    #[tokio::test]
    async fn plan_mode_pseudo_tool_call_reprompt_is_bounded() {
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();
        for _ in 0..crate::agent::runloop::unified::planning_workflow_state::MAX_PLAN_PSEUDO_TOOL_CALL_REPROMPTS {
            ctx.plan_session.mark_plan_pseudo_tool_call_reprompt_used();
        }

        let outcome = ctx
            .handle_text_response(BROKEN_MARKUP_RESPONSE.to_string(), Vec::new(), None, None, false)
            .await
            .expect("text response should be handled");

        assert!(
            matches!(outcome, TurnHandlerOutcome::Break(_)),
            "an exhausted reprompt budget must end the turn instead of looping"
        );
        let assistant_texts: Vec<String> = ctx
            .working_history
            .iter()
            .filter(|message| message.role == uni::MessageRole::Assistant)
            .map(|message| message.content.as_text().into_owned())
            .collect();
        assert!(
            assistant_texts.iter().all(|text| !text.contains("<tool_call")),
            "even with the budget exhausted, raw markup must be stripped from the final answer: {assistant_texts:?}"
        );
    }

    #[tokio::test]
    async fn build_mode_pseudo_tool_call_markup_keeps_existing_behaviour() {
        let mut backing = TestTurnProcessingBacking::new(4).await;
        let mut ctx = backing.turn_processing_context();

        let outcome = ctx
            .handle_text_response(BROKEN_MARKUP_RESPONSE.to_string(), Vec::new(), None, None, false)
            .await
            .expect("text response should be handled");

        assert!(
            matches!(outcome, TurnHandlerOutcome::Break(_)),
            "outside planning, text responses still end the turn (no new reprompt path)"
        );
    }

    #[tokio::test]
    async fn denied_interview_without_ready_plan_replaces_prose_with_hint() {
        // When request_user_input is permanently denied (non-interactive
        // runtime), no plan was proposed, no plan is persisted, and the model
        // emits research prose (not a clarifying question), the prose must be
        // replaced with the no-approval-ready-plan hint so the user gets an
        // actionable message instead of rambling text.
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();
        ctx.plan_session.mark_interview_denied();

        let outcome = ctx
            .handle_text_response(
                "I looked at the codebase and found several files.".to_string(),
                Vec::new(),
                None,
                None,
                false,
            )
            .await
            .expect("text response should be handled");

        // The first denied-interview response gets a bounded synthesis retry
        // (plan_synthesis_retry_allowed is true on the first attempt).
        assert!(
            matches!(outcome, TurnHandlerOutcome::Continue),
            "first denied-interview response without a plan should retry synthesis"
        );
        let directive_present = ctx.working_history.iter().any(|message| {
            message.role == uni::MessageRole::System && message.content.as_text().contains("Emit exactly one compact")
        });
        assert!(directive_present, "a plan-synthesis retry directive should be pushed");
    }

    #[tokio::test]
    async fn denied_interview_exhausted_retry_ends_with_hint_not_prose() {
        // After the bounded retry is exhausted, a second prose response must
        // end the turn with the no-approval-ready-plan hint, not the model's
        // research prose.
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();
        ctx.plan_session.mark_interview_denied();
        ctx.plan_session.mark_plan_synthesis_retry_used();

        let outcome = ctx
            .handle_text_response("I found more files to examine.".to_string(), Vec::new(), None, None, false)
            .await
            .expect("text response should be handled");

        assert!(
            matches!(outcome, TurnHandlerOutcome::Break(_)),
            "after retry exhaustion, prose without a plan should end the turn"
        );
        let last = ctx
            .working_history
            .iter()
            .rev()
            .find(|m| m.role == uni::MessageRole::Assistant)
            .expect("an assistant message must be pushed");
        let text = last.content.as_text();
        assert!(
            text.contains("no approval-ready plan was produced"),
            "the no-approval-ready hint must replace prose: {text}"
        );
        assert!(
            !text.contains("I found more files"),
            "the model's research prose must NOT be the final answer: {text}"
        );
    }

    #[tokio::test]
    async fn denied_interview_preserves_clarifying_question() {
        // A clarifying question (text ending with '?') is the text-mode
        // equivalent of the unavailable interview modal. It must NOT be
        // replaced with the hint or trigger a synthesis retry — the turn ends
        // so the user can answer it (checkpoint turn_856).
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();
        ctx.plan_session.mark_interview_denied();

        let outcome = ctx
            .handle_text_response(
                "Should I focus on the launch path or the config loading?".to_string(),
                Vec::new(),
                None,
                None,
                false,
            )
            .await
            .expect("text response should be handled");

        assert!(
            matches!(outcome, TurnHandlerOutcome::Break(_)),
            "a clarifying question should end the turn for user input, not retry"
        );
        let last = ctx
            .working_history
            .iter()
            .rev()
            .find(|m| m.role == uni::MessageRole::Assistant)
            .expect("an assistant message must be pushed");
        let text = last.content.as_text();
        assert!(
            text.contains("Should I focus on the launch path or the config loading?"),
            "the clarifying question must be preserved verbatim: {text}"
        );
        assert!(
            !text.contains("no approval-ready plan was produced"),
            "the hint must NOT replace a clarifying question: {text}"
        );
    }

    #[tokio::test]
    async fn planless_planning_turn_ends_with_hint_in_history() {
        // Checkpoint turn_912: after the validation repair was consumed, the
        // model answered with a planless status echo and the turn closed with
        // no visible next step. The assistant's stored answer must carry the
        // no-approval-ready hint so the user knows planning is still active.
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();
        ctx.plan_session.mark_plan_validation_repair_used();

        let outcome = ctx
            .handle_text_response(
                "Planning workflow is active with read-only permissions.".to_string(),
                Vec::new(),
                None,
                None,
                false,
            )
            .await
            .expect("text response should be handled");

        assert!(matches!(outcome, TurnHandlerOutcome::Break(_)), "a planless planning response should end the turn");
        let last = ctx
            .working_history
            .iter()
            .rev()
            .find(|m| m.role == uni::MessageRole::Assistant)
            .expect("an assistant message must be pushed");
        let text = last.content.as_text();
        assert!(
            text.contains("no approval-ready plan was produced"),
            "the no-approval-ready hint must accompany the planless answer: {text}"
        );
        assert!(
            text.contains("Planning workflow is active with read-only permissions."),
            "the model's own text must be preserved, not replaced: {text}"
        );
    }

    #[tokio::test]
    async fn planless_planning_clarifying_question_ends_without_hint() {
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();

        let outcome = ctx
            .handle_text_response(
                "Should I focus on the runtime loop or startup?".to_string(),
                Vec::new(),
                None,
                None,
                false,
            )
            .await
            .expect("text response should be handled");

        assert!(matches!(outcome, TurnHandlerOutcome::Break(_)));
        let last = ctx
            .working_history
            .iter()
            .rev()
            .find(|m| m.role == uni::MessageRole::Assistant)
            .expect("an assistant message must be pushed");
        let text = last.content.as_text();
        assert!(
            !text.contains("no approval-ready plan was produced"),
            "a clarifying question must not gain the hint: {text}"
        );
    }

    #[tokio::test]
    async fn planless_planning_hint_is_not_stored_when_turn_continues() {
        // Regression: the hint must be deferred until terminality is known.
        // A pseudo-tool-call response that triggers a bounded reprompt returns
        // `Continue` — storing the hint before that decision would leave stale
        // "no approval-ready plan" guidance in history for a turn that kept
        // going.
        let mut backing = TestTurnProcessingBacking::new(4).await;
        backing.enable_planning();
        let mut ctx = backing.turn_processing_context();

        let outcome = ctx
            .handle_text_response(BROKEN_MARKUP_RESPONSE.to_string(), Vec::new(), None, None, false)
            .await
            .expect("text response should be handled");

        assert!(
            matches!(outcome, TurnHandlerOutcome::Continue),
            "pseudo-tool-call markup should re-prompt (Continue), not end the turn"
        );
        let assistant_texts: Vec<String> = ctx
            .working_history
            .iter()
            .filter(|message| message.role == uni::MessageRole::Assistant)
            .map(|message| message.content.as_text().into_owned())
            .collect();
        assert!(
            assistant_texts
                .iter()
                .all(|text| !text.contains("no approval-ready plan was produced")),
            "a continued turn must never store the terminal hint: {assistant_texts:?}"
        );
    }

    #[test]
    fn no_ready_plan_hint_predicate_gates_on_denied_interview_and_questions() {
        assert!(should_render_no_ready_plan_hint(false, "Here is a research summary."));
        assert!(!should_render_no_ready_plan_hint(true, "Here is a research summary."));
        assert!(!should_render_no_ready_plan_hint(false, "Which approach should I take?"));
    }

    #[test]
    fn clarifying_question_detection_keeps_formatting_edge_cases_explicit() {
        let structured_plan = "## Assumptions\n1. Preserve the quoted requirement: \"Should we keep this?\"";
        assert!(!looks_like_clarifying_question(structured_plan));

        let quoted_question = "The requirement is \"Should we keep this?\"";
        assert!(!looks_like_clarifying_question(quoted_question));

        // Keep the current terminal-line heuristic documented until a corpus
        // of production false positives justifies a more semantic classifier.
        assert!(looks_like_clarifying_question("This is rhetorical: why change it?"));
    }

    #[test]
    fn rejected_plan_draft_is_reattached_to_last_assistant_message() {
        let mut history = vec![
            uni::Message::user("make a plan".to_string()),
            uni::Message::assistant("• Planning workflow is active.".to_string()),
        ];
        append_rejected_plan_draft_to_last_assistant(&mut history, "## Summary\nDo the thing");
        let last = history.last().expect("assistant message");
        let text = last.content.as_text();
        assert!(
            text.contains("Planning workflow is active")
                && text.contains("<proposed_plan>\n## Summary\nDo the thing\n</proposed_plan>"),
            "draft must be appended, not replace the stored text: {text:?}"
        );
        assert_eq!(history.len(), 2, "no new message should be pushed");
    }

    #[test]
    fn rejected_plan_draft_replaces_empty_assistant_text_and_bounds_huge_drafts() {
        let mut history = vec![uni::Message::assistant(String::new())];
        append_rejected_plan_draft_to_last_assistant(&mut history, "  ## Summary\nOnly draft  ");
        let text = history[0].content.as_text();
        assert_eq!(text, "<proposed_plan>\n## Summary\nOnly draft\n</proposed_plan>");

        let huge = "x".repeat(REJECTED_PLAN_DRAFT_HISTORY_BUDGET * 2);
        let mut history = vec![uni::Message::assistant(String::new())];
        append_rejected_plan_draft_to_last_assistant(&mut history, &huge);
        let text = history[0].content.as_text();
        assert!(text.len() < REJECTED_PLAN_DRAFT_HISTORY_BUDGET + 64);
        assert!(text.contains("…[truncated]"));

        // No assistant message → no-op, must not panic.
        let mut history = vec![uni::Message::user("hi".to_string())];
        append_rejected_plan_draft_to_last_assistant(&mut history, "draft");
        assert_eq!(history.len(), 1);
        // Empty draft → no-op.
        let mut history = vec![uni::Message::assistant("kept".to_string())];
        append_rejected_plan_draft_to_last_assistant(&mut history, "   ");
        assert_eq!(history[0].content.as_text(), "kept");
    }
}
