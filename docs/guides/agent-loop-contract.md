# Agent Loop Contract

VT Code keeps its existing harness-first runtime, but its external loop contract
now lines up more closely with SDK-style agent runtimes.

This guide describes the public lifecycle semantics shared by interactive runs,
`vtcode exec`, harness logs, and Open Responses extension events.

## Message and Event Mapping

VT Code does not expose Claude-specific SDK structs. The canonical stream stays
`vtcode_exec_events::ThreadEvent`.

The closest concept mapping is:

| Agent SDK concept | VT Code event |
| --- | --- |
| `SystemMessage(init)` | `thread.started` |
| `AssistantMessage` | `item.*` with `agent_message`, `reasoning`, `tool_invocation` |
| Tool-result `UserMessage` | `item.*` with `tool_output` or `command_execution` |
| `StreamEvent` | `item.updated` plus Open Responses stream events |
| `ResultMessage` | `thread.completed` |
| `compact_boundary` | `thread.compact_boundary` |
| Fresh plan execution handoff | `context.reset` |

`turn.started`, `turn.completed`, and `turn.failed` remain VT Code turn
wrappers around the inner item lifecycle.

## Terminal Thread Result

VT Code now emits `thread.completed` at the end of a session or exec run.

Fields:

- `thread_id`: stable event-stream thread identifier
- `session_id`: stable VT Code session identifier
- `subtype`: `success`, `error_max_turns`, `error_max_budget_usd`, `error_during_execution`, or `cancelled`
- `outcome_code`: VT Code-specific terminal code
- `result`: final assistant summary text on successful completion only
- `stop_reason`: provider stop reason when available
- `usage`: aggregate token usage for the full thread
- `total_cost_usd`: aggregate estimated cost when pricing metadata exists
- `num_turns`: total turn count

For `vtcode exec`, `outcome_code` comes from `TaskOutcome::code()`. Interactive
sessions preserve the corresponding VT Code session end semantics.

## Compaction Boundary

Whenever VT Code compacts history itself or via a provider-native compaction
path, it emits `thread.compact_boundary`.

Fields:

- `thread_id`
- `trigger`: `manual` or `auto`
- `mode`: `local` or `provider`
- `original_message_count`
- `compacted_message_count`
- `history_artifact_path`: optional archived history path
- `previous_segment_id` and `new_segment_id`: optional cache segment transition
- `previous_prefix_hash` and `new_prefix_hash`: optional immutable prompt-prefix hashes
- `previous_catalog_hash` and `new_catalog_hash`: optional ordered tool-catalog hashes

Each request segment freezes one system prompt, instruction digest, and
deterministically ordered tool catalog. Ordinary turns only append messages.
Compaction, instruction changes, catalog expansion, or primary
model/provider/mode changes start one new segment; the immutable event archive
is retained and local compaction seeds the new segment with its summary and
continuity tail.

This is emitted for manual `/compact` flows, for automatic compaction, and for
automatic local fallback compaction. When Open Responses is enabled, VT Code
surfaces these as VT Code custom extension events without changing the core Open
Responses response model.

## Fresh Plan Execution Context

Selecting “Yes, clear context and implement” is a plan-to-build handoff inside the same user
session. The runtime preserves the approved plan, task tracker, working tree, configuration,
permissions, authentication, and aggregate usage. It clears only the live transcript and other
transient continuation, recovery, cache-lineage, request-segment, and tool-budget state, then
starts a normal build turn with a compact handoff directive.

The successful handoff emits `context.reset` with `trigger`, `plan_preserved`,
`previous_context_usage_percent`, and `tool_budget_reset`. The event is also written to the normal
JSONL/log stream and forwarded by the Open Responses bridge as `vtcode.context_reset`.

### Unified auto-compaction

Auto-compaction is **on by default** (`agent.harness.auto_compaction_enabled`,
default `true`) and is **unified across both runloops**: the core `AgentRunner`
loop and the binary unified runloop both delegate to the shared
`vtcode_core::compaction` orchestrator (`auto_compact_messages`) rather than
maintaining separate compaction logic. It fires when the live token usage
crosses `agent.harness.auto_compaction_threshold_tokens` (or a context-size
ratio default).

To preserve conversational continuity, every compacted history keeps:

- a **continuity tail** — approximately 20,000 estimated tokens of the newest
  complete user/assistant/tool protocol groups retained verbatim. Incomplete
  trailing tool calls are dropped, and an oversized individual message is
  represented by a bounded preview/spool reference;
- the structured **session memory envelope** injected at the boundary (see
  Resume and fork continuity).

The soft compaction threshold is 90% of the effective hard threshold. Reaching
it marks compaction pending and defers the work to the next outer turn
boundary. The hard threshold compacts before the next model request. No hidden
summary model call is issued in the middle of an active tool loop.

ACP sessions use the same compaction orchestrator before provider admission.
The ACP prompt budget includes resolved system and session messages plus tool
definitions, and reserves output capacity and a safety margin from the
provider's effective context window. When compaction changes the history, ACP
replaces the live thread and persists a durable checkpoint before rebuilding
the provider request.

The fork/branch history builder (`build_summarized_fork_history`) deliberately
omits the continuity tail and produces a minimal resume artifact (envelope +
summary + retained users only).

### Long-running command waits and durable steering

`write_stdin` and `unified_exec` accept an explicit `action: "wait"`. A wait
blocks until the process exits or its requested deadline expires, then returns
one bounded result. A deadline does not terminate an in-progress process; the
returned session ID is reusable for a later wait. Wait time is excluded from
the ordinary per-turn harness wall-clock budget, while cancellation, shutdown,
safety policy, and the configured long-running-command ceiling remain active.

Command responses never expose unbounded accumulated `raw_output`. They return
a bounded preview plus total bytes, truncation state, exit state, and a spool
path when the output file is available. `spool_complete` is false while an
active session is still writing and the path is a readable partial snapshot.
When an exited session is still draining, the path is withheld and
`spool_pending` is set until a later wait completes the output spool.

Steering follow-ups are persisted as UUID-tagged intents. The schema-v3 session
envelope keeps at most 16 pending intents and a 64-ID applied window. Recovery
replays only pending IDs absent from both the applied window and tagged user
history; the public `FollowUpInput(String)` message shape remains unchanged.

## Budget and Limits

`agent.harness.max_budget_usd` is the shared budget setting for interactive and
exec sessions.

- VT Code estimates cost from aggregate usage via `ModelResolver::estimate_cost`.
- If pricing metadata is unavailable for the active model, VT Code does not
  enforce the budget.
- In that case `total_cost_usd` stays `null` and VT Code emits one warning.

Turn limits still surface through `thread.completed.subtype = "error_max_turns"`.

## Hooks

VT Code now supports `hooks.lifecycle.pre_compact`.

`pre_compact` runs before VT Code records a compaction boundary. Its payload
includes:

- `session_id`
- `cwd`
- `hook_event_name = "PreCompact"`
- `trigger`
- `mode`
- `original_message_count`
- `compacted_message_count`
- `history_artifact_path`
- `transcript_path`

`session_start` with source `compact` remains supported for compatibility, but
`pre_compact` is the first-class hook for compaction-aware automation.

## Orient Phase

Every session should begin by gathering orientation context from external artifacts. This follows the long-running harness pattern: the agent reads the progress ledger, harness artifacts, loop memory, and git log to understand the current state before acting.

The orient phase produces an `OrientationContext` (see `crates/codegen/vtcode-core/src/core/agent/bootstrap.rs`) that includes:

- Progress ledger summary (goal, completion ratio, confidence, stall status)
- Harness artifact summaries (spec, contract, sprint contract, evaluation, outcome verification)
- Recent git log (last 5 commits)
- Loop memory notes and decisions from previous iterations
- Handoff context from a previous agent, if any

This context is injected as a `[Orientation Context]` section in the system prompt, using summaries and references rather than full content to keep the context lean.

## Handoff Protocol

When one agent hands off to another, it produces a `HandoffRequest` (see `crates/codegen/vtcode-core/src/core/agent/handoff.rs`) that includes:

- **State summary**: what was accomplished, what remains
- **Boundary status**: explicit list of features/deliverables with Done/InProgress/NotStarted/Blocked status
- **Modified files**: files changed in this session
- **Test results**: last test run outcome with actual output
- **Open decisions**: unresolved questions for the next agent
- **Known issues**: bugs, limitations, tech debt the next agent should know
- **Next actions**: recommended next steps
- **Task context**: the original task description

The handoff prompt is rendered as a structured markdown section that the next agent can parse without re-exploring the codebase. This prevents the "inheriting a collaborator's mess" problem: the boundary status makes explicit what is done vs. what was left incomplete.

## Related Controls

These VT Code settings line up with common agent-loop controls:

- Tool allow and deny rules: `[permissions].allow`, `[permissions].deny`, tool policy config
- Permission policy: workspace trust, human-in-the-loop settings, granular agent rules, and full automation allow-lists
- Effort: provider/model reasoning settings
- Tool discovery: MCP and tool catalog flows
- Resume and fork continuity: session archives, thread bootstrap, and compaction envelopes

## Context Reset

Context reset is a context engineering technique **distinct from compaction**.
While compaction preserves conversational continuity within the same task,
context reset deliberately discards conversation history so a fresh agent can
reorient from durable artifacts only.

### When It Triggers

Configured via `agent.harness.context_reset_mode`:

| Mode | Trigger | Use Case |
|------|---------|----------|
| `off` (default) | Never | Normal operation |
| `on_stall` | `context_reset_stall_threshold` consecutive stalled turns | Long-horizon tasks where the agent gets stuck |
| `on_compaction` | After every auto-compaction | Clear noise accumulated before compaction |

### What Happens

When a reset triggers:

1. A `ContextResetManifest` is written to `.vtcode/tasks/current_context_reset.md`
   recording the trigger reason, stall count, and timestamp.
2. The next session starts with **only** `OrientationContext` — no conversation
   history is carried forward.
3. The orient phase reads the manifest and prepends a `### Context Reset` banner:
   "This session starts from a clean context. Reorient from the artifacts below."

### Artifacts That Survive a Reset

All durable artifacts persist across a reset:

- Progress ledger (`crates/codegen/vtcode-memory/src/progress.rs`)
- Harness artifacts (spec, contract, feature list, evaluation, sprint contract)
- Loop memory (notes, decisions)
- Git log and working tree state
- Compaction summary

The comparison with compaction is summarised in [Context Reset](#context-reset).

## Loop Engineering Additions

The subagent layer now supports loop-engineering primitives:

- **Worktree isolation**: set `isolation = "worktree"` on an agent spec to run the child in a git worktree under `.vtcode/worktrees/`. The child's file mutations stay in its own working tree until explicitly merged.
- **Propose/verify separation**: `SubagentController::verify_proposed_change()` spawns a read-only verifier sub-agent that re-reads affected files and approves or rejects the change. The verifier has no shared context with the proposer.
- **Loop run state**: `crates/codegen/vtcode-core/src/loop_state.rs` persists step index, cumulative cost, and status to `.vtcode/state/loop-<id>.json` so a scheduler can resume across invocations.
- **Loop memory**: `crates/codegen/vtcode-core/src/loop_memory.rs` provides an append-only store for agent notes and decisions in `.vtcode/state/notes.md` and `decisions.md`.
- **Cost guardrails**: `CostBudget` in `loop_state.rs` tracks token/cost/step limits and reports `BudgetStatus` (Ok/TokenLimitReached/CostLimitReached/StepLimitReached).

See [Loop Engineering](../loop-engineering.md) for the full design.
