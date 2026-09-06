# vtcode Binary Gotchas

This guide preserves detailed maintainer notes for the binary crate. Keep
`src/AGENTS.md` concise; use this page when changing the runloop, update path,
allocator, or request assembly.

## Startup, updates, and allocation

- TUI shutdown ownership: the spawned TUI task (`spawn_session_with_options`) owns terminal restoration via `panic_hook::restore_tui()`. `finalize_session` must await `InlineSession::wait_for_exit()` after `handle.shutdown()` before calling `restore_terminal_on_exit()` as a backstop — `restore_tui()` is idempotent through the global `RESTORE_DONE` flag, so whoever claims it first wins, and a host restore issued on a fixed sleep races the TUI's final frames onto the main screen (transcript leaks into CLI scrollback). The TUI side mirrors this with `is_restore_claimed()` guards and the shared terminal-operation lock: `drive_terminal` exits without rendering, `render_if_dirty` rechecks the claim while holding the lock, and `run_tui` skips `finalize_terminal` once restore is claimed.
- Standalone updates own asset selection, checksum verification, safe archive extraction, and `self_replace`; keep managed-install guidance separate and keep inline TUI downloads quiet.
- `main_helpers` handles runtime relaunch context; do not duplicate initialization logic in `main.rs`.
- Allocator memory pinning: vtcode's bursty/sparse Tokio workload (semaphore-capped `JoinSet` fan-out followed by idle workers) can leave both `mimalloc` and `glibc` RSS flat after a burst because frees are stranded on cross-thread lists until later allocation activity. `jemalloc` returns memory while idle only when `background_thread` is active; on macOS its build reports `background_thread currently supports pthread only` and behaves like mimalloc. On Linux containers it can reclaim memory. Measure with `vtcode bench-allocator` before changing the default.
- `load_dotenv()` must run before config load so `.env` API keys are available.
- Provider noise (for example `]<]minimax[>[`) is stripped centrally in `turn::provider_noise`; stream-level harmony and MiniMax sanitization lives in `stream_sanitization::StreamSanitizer`. Do not re-implement it inline.

## Tool-budget and recovery contracts

- Wall-clock and tool-call budget exhaustion share one contract: emit the full policy message once, compact stubs for later batch calls, then one synthesis directive via `flush_budget_synthesis_directives` after the tool batch. Do not break the turn as `Blocked`, because that skips synthesis. The flush arms `switch_to_tool_free_recovery()` so the next request removes tool definitions at the API level. Exclude explicit `request_user_input` think-time and command-session `wait` time from the wall-clock calculation; neither consumes autonomous execution budget.
- Anti-blind editing is session-scoped: once successful mutations require verification, a blocked turn and later `continue` retain the gate. Only a successful standalone verification-classified command clears it; reads, diffs, and ordinary stall recovery do not. A failed verifier run (non-zero exit) keeps the gate but grants 2 fix-up edits (`FAILED_VERIFICATION_FIX_ALLOWANCE`, persisted in `SessionStats`) so a broken build can be repaired before re-verify; tool-level failures that never executed grant no window. Truncation-only piped verifiers (`cargo check 2>&1 | head`) are admitted to run but never clear the gate — run verifiers standalone; chained mutations smuggled behind a verifier prefix (`cargo check && rm …`) stay blocked because every shell segment must be verification-or-readonly (`shell_command_is_admitted_verification_attempt`). Planning synthesis (`<proposed_plan>` or planning-active text) is exempt from the pending-verification text block because it makes no workspace mutation. Copilot/direct execution and batch execution must persist the `SessionStats::verification_snapshot` bundle immediately after updating their `LoopTracker`, so resumed turns reconstruct the same gate state.
- If the ordinary tool-loop allowance is reached and no manual extension is granted, schedule one tool-free synthesis request before ending the turn. The loop allowance becomes unlimited only for that recovery control pass; the provider request still has tools disabled. The absolute hard cap and its terminal behaviour remain unchanged.
- `tool_budget_exhausted_emitted` participates in the plan-mode `mark_recovery_exhausted` gates. The preflight circuit breaker in `turn/tool_outcomes/handlers/mod.rs` follows the same `Continue` plus deferred synthesis path via `preflight_circuit_recovery_pending` and `flush_preflight_circuit_recovery`; never return `Break(Blocked)` for an approved-plan build.
- Approved-plan implementation turns receive one internal `+50` loop allowance at initialization, clamped by the ordinary hard cap; `0` remains unlimited and manual extensions/tool-call budgets stay independent.
- Queue model-facing auto-permission probe warnings while a tool batch runs. Flush them after all tool results and before recovery directives; keep the UI warning immediate and deduplicated.
- During tool-free recovery, do not inject `request_user_input`; it violates the recovery contract. Plan-aware fallback must preserve interview state, while exhausted budget or recovery caps conclude with the user-facing planning notice instead of re-forcing research or emitting a final directive with no following model call.
- In planning, two consecutive empty model responses schedule exactly one tool-free synthesis from the latest request and bounded evidence. Require one validated canonical `<proposed_plan>` block; empty, malformed, tool-bearing, or unpersistable synthesis emits `turn.failed` with an actionable resumable handoff and keeps planning active.
- Canonical event finalization is independent of optional Open Responses, ATIF, legacy, and WebMCP exporters: a blocked turn emits `turn.failed`, while `thread.completed` is reserved for final shutdown. Exporter failures are diagnostics and must not prevent the canonical event path from closing.
- The assistant text-response cap applies to consecutive text-only responses, not the cumulative number emitted in a turn. The authoritative turn-state streak survives compaction and Copilot inline execution, which is not represented in ordinary message history. Reset it only after a tool passes admission; blocked, preflight-invalid, and verification-gated attempts are not progress and retain the streak while their dedicated safeguards bound those failure loops. Copilot must also record out-of-band tool progress so a failed post-tool synthesis receives the same bounded recovery as history-backed tool execution. Reaching the cap is a blocked safety stop, not successful completion. If only substantive commentary exists at the cap, promote the latest commentary to the final history phase and reuse the existing renderer/stream flags so the user and canonical `ThreadEvent` stream receive exactly one preserved response; use the deterministic fallback only when no substantive response exists.
- The interview-denied recovery fallback must distinguish "no draft produced" from "draft persisted." `PLANNING_RECOVERY_SYNTHESIS_FALLBACK_NO_INTERVIEW` promises "Review the plan below" and offers `yes`/`implement`/`no`/`edit` — those choices dead-end without a persisted plan and contradict the appended `PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT`. When `persisted_plan_ready` is false, use `PLANNING_INTERVIEW_DENIED_NO_DRAFT_NOTICE` instead (turn_902).
- Permanent `request_user_input` denial is distinct from user cancellation. Mark denial from both execution failure and permission-flow denial, suppress the tool for the rest of the session, and route text replies through the shared planning intent classifier.

## WebMCP bridge boundary

- WebMCP stays opt-in with explicit origins, loopback defaults, in-memory credentials, terminal/full-auto authorization, and the existing full-auto workspace-trust gate before headless mutations/checks. Active bridge replacement and unpair require terminal confirmation.
- Bridge prompts use a prompt-only inline event and must not enter slash-command parsing. Keep browser authority separate from terminal-owned origins, roots, pairing, and policy.

## Request assembly and planning

- `turn_processing/llm_request/` uses contract-carrying `pub(super)` submodules (`snapshot`, `tool_shaping`, `context_management`, `response_chain`, `prompt_assembly`, `prompt_sections`, and `prompt_runtime`). Go through `mod.rs` exports.
- `ContextManager::normalize_history_for_request` is request-scoped: preserve the borrowed fast path for clean histories and use the core normalizer only to repair provider-facing tool-result ordering. Never mutate durable session history as part of provider retry.
- Prompt section order in `prompt_assembly::build_prompt_output` is provider-cache-sensitive. Hosted Anthropic/OpenAI payloads retain deferred tool definitions; only ClientLocal policy omits them. `metrics.rs` emits the per-request `token_budget_breakdown`; prompt-cache diagnostics remain in `SessionStats`.
- Plans stay compact/spec-like: `PLANNING_WORKFLOW_PLAN_QUALITY_LINE` requires roughly 1500 tokens with `Action -> files/symbols -> verify:` steps and file/symbol references. The truncation recovery prompt is bounded by `MAX_PLAN_SYNTHESIS_CONDENSE_ATTEMPTS`.
- Planning research scales to request complexity; the wall-clock budget is the backstop, not a replacement hard tool-count cap. `effective_max_tool_calls_for_turn` and mid-turn config reapplication preserve the shared planning floor.
- Approval phrases are classified by `detect_planning_intent` and must restore a write-capable primary agent, preserve `auto_accept`, create the tracker, and apply the approved-plan tool-call floor. Direct and queued approval paths share this boundary. The implementation turn is an explicit internal next-turn trigger, not a best-effort steering item; append its synthetic user prompt exactly once after the handoff directive so a full steering FIFO cannot leave the build agent waiting for `continue`.
- Emit `plan.approval.requested` and `plan.approval.resolved` for every decision using the canonical `ThreadEvent` contract and preserve Open Responses parity.
- Enter/exit phrase literals live in `vtcode_core::planning` and are shared by the runloop and Codex bridge; add a phrase once, never fork consumer-specific aliases.
- Tool-summary renderers receive `ToolSummaryRenderContext` at their public render entry points; pure helpers may remain `Option<&Path>`-driven internally.
- Compact tool summaries are session-local; compact mode is the default, contiguous successful command calls share an activity row, and bounded live PTY output remains separate from complete captures. Failures, warnings, diffs, stderr, and result bodies retain their boundaries. Complete command output is available in Transcript Review through the configured primary binding (default `Ctrl+T`), interleaved with the rest of the conversation. Rich review mode is default and the configured render binding (default `R`) switches to ANSI-free raw text; the visible review hint, close button, and shortcut guide are configurable.
- Ordinary completed turns publish a non-empty final assistant response through both renderer and `ThreadEvent` harness paths. The approved-plan handoff is the control-flow exception: its outer loop creates the implementation request, so it must remain `Completed { plan_approved_execution_pending: true }` without synthesizing a final response. Recovery fallbacks remain visible but produce a blocked outcome; approved-plan summaries retain changed files, verification, and blockers.

For prompt/runtime source boundaries, see [runtime guidance](./runtime-guidance.md).

## Startup benchmark guidance

The standalone startup matrix is deliberately provider-free:

```text
vtcode --version
vtcode --help
vtcode schema tools --format ndjson --name code_search
```

Build and invoke it with the release executable:

```bash
cargo build --release --locked --bin vtcode
VTCODE_BIN="$PWD/target/release/vtcode" \
  VTCODE_BENCH_RUNS=10 cargo bench --locked --bench startup -- --noplot
```

Measure both warm launches and cold fresh-executable-copy launches. A cold
sample copies the executable to a new temporary path for each process; it is a
loader/process cold proxy, not an OS page-cache benchmark. The harness does
not flush or evict OS page caches. Isolate each child with temporary `HOME`,
config, data, explicit config-file, and workspace paths so credentials and
developer state cannot influence startup.

Results must retain raw samples and report median and p95 per case and mode.
Keep the benchmark environment, binary, and sample count fixed when comparing
changes. `VTCODE_STARTUP_TRACE=1` is for a separate diagnostic run only; it
emits bootstrap phase durations to stderr without changing normal CLI output.
Metadata commands report `dispatch_ready`, while the interactive path reports
its first rendered frame separately.
