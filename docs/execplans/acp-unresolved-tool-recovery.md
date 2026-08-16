# Recover interrupted ACP tool calls safely

This ExecPlan (execution plan) is a living document. The sections
`Constraints`, `Tolerances`, `Risks`, `Progress`, `Surprises & Discoveries`,
`Decision Log`, `Outcomes & Retrospective`, `Conformance Basis`, and
`Verification Plan` must be kept up to date as work proceeds.

Status: IN PROGRESS

## Purpose / big picture

After this change, an ACP session can be interrupted during a tool call and
resumed without leaving the model with an invalid assistant tool request that
has no corresponding Tool result. VTCode will write an incomplete terminal
result before starting each tool, replace it with the real result on normal
completion, repair old archives that predate this write-ahead behaviour, and
tell the resumed model that it must inspect the workspace and issue a new call
instead of assuming or replaying an uncertain mutation.

The same branch will make a stale `write_stdin` session identifier an explicit
non-retryable resource-not-found failure. Its message will say to start a new
command rather than claiming that partial changes may exist.

This is the third layer in the VTCode pull-request stack. It starts from
`feat/model-visible-file-versions-noop-patch-guard` (draft PR #12), which in
turn starts from `fix/acp-custom-provider-routing` (draft PR #11). Thus its
release build contains the provider, ACP, patch-version, and no-op-guard fixes
from both lower layers.

## Constraints

- Do not automatically execute or replay a tool call recovered from durable
  history. A mutating call may already have produced side effects before the
  process failed.
- Preserve the provider message invariant that every assistant tool-call ID is
  followed by exactly one Tool result before later user or assistant messages.
- Keep the persisted `SessionMessage` JSON shape backwards compatible. Legacy
  archives must load without an offline migration.
- Use `vtcode-exec-events::ThreadEvent` for runtime events; do not introduce a
  parallel event contract.
- Do not hold a synchronous lock across `.await` and do not create an
  unsupervised background task solely for recovery persistence.
- Preserve ACP 1.0.1 wire compatibility and use the pinned official Rust SDK
  duplex harness for behavioural tests.
- Add no new external dependency. Use the existing `proptest`, Tokio, and ACP
  test dependencies.
- Keep the plan and documentation in the repository, and install the exact
  final release binary at user level after every implementation commit.

## Tolerances (exception triggers)

- Scope: stop if the implementation needs more than 12 production files or
  900 net new production lines, excluding tests, documentation, and this plan.
- Interface: stop if an existing public Rust API or ACP wire schema must change.
- Persistence: stop if recovery requires rewriting archives other than through
  the existing atomic checkpoint path.
- Dependencies: stop if a new crate is required.
- Iterations: stop if a focused red test remains unexplained after three
  implementation attempts.
- Concurrency: stop if preventing the race requires serialising prompts across
  different ACP sessions rather than only within one session.
- Ambiguity: stop if evidence shows that the client expects automatic replay;
  that would conflict with the explicit safety requirement.

## Risks

- Risk: a placeholder Tool result could leak into a normal provider request
  before the actual result replaces it.
  Severity: high.
  Likelihood: low.
  Mitigation: retain the existing per-thread turn guard, stage and checkpoint
  placeholders only inside the active turn, and replace them before the next
  provider request. Add a normal-completion regression proving only real
  results reach the continuation request.

- Risk: legacy repair could duplicate a valid result or reorder history.
  Severity: high.
  Likelihood: medium.
  Mitigation: make repair a pure, idempotent transformation and use property
  tests over generated tool-call groups, partial result subsets, and unrelated
  messages.

- Risk: a process can fail after a tool side effect but before its real result
  is checkpointed.
  Severity: high.
  Likelihood: medium.
  Mitigation: the durable write-ahead result explicitly marks state as
  uncertain, prohibits replay, and instructs the model to verify the workspace.
  This is intentional conservative recovery, not an attempt at distributed
  transaction semantics.

- Risk: the current turn guard may reject a second prompt only after doing
  ancillary work such as task-plan replay.
  Severity: medium.
  Likelihood: high.
  Mitigation: acquire the guard before any prompt-scoped tool or history work,
  and prove ordering through the official ACP duplex harness.

- Risk: generic command classification may overwrite a typed stale-session
  classification and re-enable the misleading partial-state suffix.
  Severity: medium.
  Likelihood: medium.
  Mitigation: preserve the typed classification through
  `ToolExecutionError::with_tool_call_context` and assert the complete
  structured and user-facing result.

## Progress

- [x] (2026-08-16) Falsified the hypothesis that `apply_patch` reported
  success without writing. The two problematic archive calls had no matching
  Tool message; ordinary successful calls had paired results and changed files.
- [x] (2026-08-16) Published the completed file-version/no-op-guard base as
  draft PR #12 and created `fix/acp-unresolved-tool-recovery` from its exact
  head.
- [x] (2026-08-16) Located the prompt turn guard, tool-call checkpoint gap,
  durable archive attachment path, and generic stale-session error path.
- [ ] EP-M1: add red unit, property, and ACP wire tests for write-ahead tool
  results, legacy repair, no replay, and prompt ordering.
- [ ] EP-M2: implement ACP write-ahead terminalisation and durable repair.
- [ ] EP-M3: implement typed stale `write_stdin` classification and focused
  error tests.
- [ ] EP-M4: update user/developer documentation, run all commit gates, install
  the release binary, push the branch, and create the stacked draft PR.

## Surprises & discoveries

- Observation: `run_prompt` already holds a `TurnGuard`, but it calls
  `replay_persisted_task_plan` before acquiring that guard.
  Evidence: `crates/codegen/vtcode-acp/src/zed/agent/handlers.rs::run_prompt`.
  Impact: a concurrent prompt is rejected before history mutation but can still
  perform ancillary tool work. Guard acquisition must move to the first
  session-scoped operation.

- Observation: the live tool loop checkpoints the assistant tool-call message
  before executing tools, and checkpoints Tool results only after all calls
  return.
  Evidence: the `assistant_tool_calls` and `tool_results` checkpoint boundaries
  in `run_prompt`.
  Impact: a process failure between those boundaries creates the exact invalid
  durable history observed in the user archives. The assistant call and its
  recovery results must become one write-ahead checkpoint.

- Observation: graceful `execute_tool_calls` errors already append interruption
  results, but task abort, process death, or a connection loss can bypass that
  caller cleanup.
  Evidence: `execute_tool_calls` returns only after sequential execution and
  the caller owns error-message insertion.
  Impact: exception-path cleanup remains useful, but durable safety must not
  depend on the future reaching that cleanup block.

- Observation: `with_tool_call_context` marks every command tool as possibly
  mutating, even when a typed pre-execution lookup proves that a
  `write_stdin` session does not exist.
  Evidence: `crates/codegen/vtcode-core/src/tools/registry/error.rs`.
  Impact: stale-session classification must survive this decoration step.

## Decision log

- Decision: use a durable write-ahead Tool result, not replay metadata or a
  pending-work queue.
  Rationale: it restores provider-valid history before a risky operation and
  naturally prohibits automatic replay. A successful tool replaces the
  placeholder before the continuation request.
  Date/Author: 2026-08-16, Codex.

- Decision: repair legacy history as a pure message transformation during
  archive attachment, then persist it through the existing checkpoint method.
  Rationale: this keeps compatibility at the deployed JSON boundary without a
  second migration store or source-API shim.
  Date/Author: 2026-08-16, Codex.

- Decision: insert a missing result immediately after the existing contiguous
  Tool results for its assistant call.
  Rationale: OpenAI-compatible providers require Tool results to remain next to
  the assistant tool request. Appending repairs at the end would still leave an
  intervening user message and an invalid conversation.
  Date/Author: 2026-08-16, Codex.

- Decision: model stale exec-session lookup as a typed internal error and map
  it to `ResourceNotFound` at the registry boundary.
  Rationale: retryability, circuit-breaker impact, partial-state claims, and
  recovery guidance must not depend on parsing an English error string.
  Date/Author: 2026-08-16, Codex.

## Outcomes & retrospective

Implementation is in progress. This section will record the final behaviour,
gate evidence, installed binary identity, pull-request URL, and any residual
gap after every conformance item has been reconciled.

## Context and orientation

`crates/codegen/vtcode-acp/src/zed/agent/handlers.rs::run_prompt` owns an ACP
prompt from admission through provider streaming, tool execution, continuation,
and final checkpointing. `TurnGuard` delegates to
`ThreadRuntimeHandle::begin_turn` and rejects a second in-flight prompt for the
same thread. `crates/codegen/vtcode-acp/src/zed/agent/tool_execution.rs` adapts
model tool calls into ACP ToolCall updates and local or client-side execution.

A provider tool request is stored as an assistant `Message` whose `tool_calls`
field contains one or more IDs. Each ID must have a later Tool-role `Message`
with the same `tool_call_id`. In the failure archives, the assistant message was
persisted, but no Tool message was ever recorded. A write-ahead Tool result is
an intentionally conservative placeholder persisted before execution. Its
content says execution is incomplete, side effects are uncertain, replay did
not occur, and the model must verify state and issue a new call if required.
Normal completion replaces that placeholder with the actual Tool result.

`crates/codegen/vtcode-acp/src/zed/agent/session_state.rs` loads durable
`SessionListing` values and builds `ThreadBootstrap` state. This is the correct
compatibility boundary for repairing old archives. The repair will be isolated
in a small ACP agent module rather than expanding the already large handler.

`crates/codegen/vtcode-core/src/tools/exec_session.rs` owns process-session
lookup. `crates/codegen/vtcode-core/src/tools/registry/error.rs` converts typed
and `anyhow` failures into the structured `ToolExecutionError` shown to the
model. Today a missing session becomes a generic execution error and later
command decoration adds `partial_state_possible = true`.

## Conformance basis

There is no separate Terms of Reference, technical design, or ADR for this
repair. The authoritative requirements are the six behaviours in the user
request dated 2026-08-16, the ACP 1.0.1 message contract supplied by the pinned
`agent-client-protocol` crate, and the repository rules in `AGENTS.md` plus the
`vtcode-acp` and `vtcode-core` module guidance.

Stable requirement identifiers are:

- ACP-REC-1: every persisted assistant tool call has a terminal Tool result.
- ACP-REC-2: legacy unresolved calls are repaired on durable load/resume.
- ACP-REC-3: recovered calls are never automatically replayed.
- ACP-REC-4: recovery text tells the model to verify state and resubmit.
- ACP-REC-5: prompt-scoped work cannot race an in-flight tool execution.
- ACP-REC-6: stale `write_stdin` IDs are non-retryable resource-not-found
  failures with no partial-state or circuit-breaker claim.

Trace links:

```plaintext
ACP-REC-1, ACP-REC-3, ACP-REC-4 -> EP-M1, EP-M2
  -> ACP duplex and tool-recovery tests
ACP-REC-2 -> EP-M1, EP-M2 -> archive-load and repair property tests
ACP-REC-5 -> EP-M1, EP-M2 -> overlapping-prompt ACP duplex test
ACP-REC-6 -> EP-M3 -> typed exec-session and structured-error tests
```

## Verification plan

- Obligation: INV-TOOL-CLOSURE. After staging or repairing a history, every
  assistant tool-call ID has exactly one contiguous Tool result before the next
  non-Tool message.
  Method: property tests plus finite examples.
  Rationale: generated subsets and orderings cover more partial histories than
  hand-written examples, while examples make provider ordering legible.
  Domain: one to eight unique calls, arbitrary subsets of completed results,
  unrelated preceding/following messages, and repeated repair operations.
  Artefact: a new tool-recovery test module under
  `crates/codegen/vtcode-acp/src/zed/agent/`.
  Evidence: focused `cargo nextest run -p vtcode-acp -E
  'test(tool_recovery)'`; red must expose at least one unresolved ID before
  production code exists, and green must pass with 1,000 or more generated
  cases.
  Non-vacuity: generators require at least one call and separately force empty,
  partial, and full result subsets. A negative-control helper that omits repair
  must fail the closure predicate.

- Obligation: INV-REPAIR-IDEMPOTENCE. Applying legacy repair twice produces
  byte-equivalent message history and never replaces an existing real result.
  Method: property test.
  Rationale: idempotence spans many message/result combinations and protects
  repeated load/resume attempts.
  Domain: the same generated histories as INV-TOOL-CLOSURE, including arbitrary
  real result text.
  Artefact and evidence: the same test module and focused nextest command.
  Non-vacuity: generated incomplete histories must change once, while complete
  histories and the second pass must not change.

- Obligation: LEMMA-WRITE-AHEAD. The durable checkpoint visible before tool
  execution contains the assistant call and incomplete Tool results; normal
  completion replaces those results before provider continuation.
  Method: deterministic unit tests with an injected blocking tool and explicit
  barriers, plus the official ACP Rust SDK duplex harness.
  Rationale: controlled barriers prove the ordering without wall-clock sleeps,
  and the duplex harness proves the real handler boundary.
  Domain: one local non-mutating test tool and one mutating-shaped call.
  Artefact: `crates/codegen/vtcode-acp/src/zed/agent/handlers.rs` tests.
  Evidence: the red test inspects the archive while the tool is blocked; green
  sees the incomplete result at that point, then only the real result after
  release.
  Non-vacuity: the test asserts the tool has actually started and that its
  completion barrier is still closed when the checkpoint is inspected.

- Obligation: INV-NO-REPLAY. Loading an archive with an unresolved mutating call
  performs zero tool executions and gives the next provider request an
  incomplete result containing explicit verify-and-resubmit guidance.
  Method: official ACP SDK duplex test with an execution-counting injected
  tool/provider seam.
  Rationale: this crosses the durable loader, handler, and provider-context
  boundaries while remaining deterministic.
  Domain: a legacy `apply_patch`-named call with no Tool result.
  Artefact: ACP wire tests in `handlers.rs` or `session_state.rs`.
  Evidence: execution count remains zero after load; captured provider messages
  contain `replay` false, uncertain-state language, and the next action.
  Non-vacuity: the injected tool increments on any execution, and a seeded
  auto-replay mutation makes the assertion fail.

- Obligation: INV-PROMPT-SERIALITY. A second prompt performs no task replay,
  history append, provider call, or tool call while the first prompt owns the
  turn.
  Method: deterministic ACP duplex concurrency test using barriers.
  Rationale: this verifies the public request boundary rather than only the
  `TurnGuard` helper.
  Domain: two prompts for one session, with the first blocked in a tool; a
  separate-session control remains independently runnable.
  Artefact: `handlers.rs` wire tests.
  Evidence: the second prompt receives `turn_in_progress`; counters and history
  are unchanged until the first releases.
  Non-vacuity: the first-tool-start barrier proves overlap; a control that moves
  guard acquisition after task replay increments the ancillary-work counter and
  fails.

- Obligation: INV-STALE-SESSION-CLASSIFICATION. A missing `write_stdin` ID is
  `ResourceNotFound`, non-retryable, recoverable by starting a new command,
  excluded from circuit-breaker failures, and marked as incapable of partial
  state.
  Method: focused typed-error unit and registry integration tests.
  Rationale: the finite state vector is small and can be exhaustively asserted.
  Domain: missing ID for write, poll, and wait dispatches, plus an existing
  session control.
  Artefact: `exec_session.rs`, `registry/error.rs`, and executor tests.
  Evidence: focused nextest filters pass and the rendered message omits
  `Partial changes may still exist`.
  Non-vacuity: an ordinary command execution error remains partial-state
  possible, proving the exception is not global.

Non-trivial axioms are that the existing checkpoint writer atomically replaces
the session archive, provider tool-call IDs are unique within a conversation,
and the pinned ACP duplex channel faithfully exercises request dispatch and
notifications. The plan verifies repository-owned use of those interfaces but
does not attempt to prove Tokio, serde, or the ACP SDK internals.

## Plan of work

EP-M1 adds the pure closure predicate, legacy repair examples, generated
idempotence/completeness properties, and deterministic ACP overlap tests before
production behaviour changes. Each focused test must fail for the named missing
contract rather than from fixture setup.

EP-M2 introduces a small `tool_recovery` module in `vtcode-acp`. `run_prompt`
will acquire its turn guard before any session-scoped work, append the assistant
tool request and write-ahead Tool messages as one in-memory transition, persist
that transition, execute the calls, replace placeholders with actual results,
and checkpoint again. The archive attachment path will repair legacy histories
before `ThreadBootstrap::from_listing`, checkpoint a changed archive, and never
invoke a recovered tool.

EP-M3 adds a typed missing-session error in `exec_session.rs` and a narrow
mapping in `registry/error.rs`. Command-context decoration will preserve this
pre-execution resource-not-found state. Tests will assert both structured JSON
and the exact actionable user message.

EP-M4 updates `docs/guides/zed-acp.md` and the relevant tool/error guidance,
runs focused tests followed by the repository commit gates through the
scrutineer, builds and installs the release binary, pushes the branch, opens a
draft PR based on `feat/model-visible-file-versions-noop-patch-guard`, and links
the three PRs as a stack when GitHub stacked PR support is available.

## Milestones and plateaus

- Identifier and outcome: EP-M1, executable red specifications exist.
  Requirements and gaps: all six requirements have a failing behavioural or
  structural test.
  Acceptance evidence: failure logs show the missing write-ahead result,
  unrepaired archive, late guard, or generic stale-session classification.
  Conformance check: no production behaviour changes; no interface or
  persistence-format deviation.
  Recovery: tests can be reverted independently.
  Remaining gaps: all production implementation.
  Compatibility decision: none; tests describe the deployed archive boundary.

- Identifier and outcome: EP-M2, ACP histories remain provider-valid across
  interruption and durable resume.
  Requirements and gaps: ACP-REC-1 through ACP-REC-5 are discharged.
  Acceptance evidence: focused unit, property, archive, and ACP duplex tests.
  Conformance check: no tool replay, no new wire type, existing checkpoint path
  retained, and turn ownership remains per session.
  Recovery: revert the milestone commit; legacy archives remain readable.
  Remaining gaps: stale `write_stdin` classification and final gates.
  Compatibility decision: persisted JSON compatibility is required for
  archives written by released/local VTCode builds; repair is additive in
  memory and then uses the normal checkpoint writer.

- Identifier and outcome: EP-M3, stale process-session IDs are actionable and
  accurately classified.
  Requirements and gaps: ACP-REC-6 is discharged.
  Acceptance evidence: typed-error and registry integration tests.
  Conformance check: only missing exec-session lookup is special-cased; other
  command failures preserve existing safety semantics.
  Recovery: revert the typed error and mapping together.
  Remaining gaps: documentation, full gates, installation, and publication.
  Compatibility decision: none; the public JSON shape is unchanged and only
  field values become more accurate.

- Identifier and outcome: EP-M4, validated stacked draft PR and installed
  aggregate build.
  Requirements and gaps: all requirements and documentation obligations are
  complete.
  Acceptance evidence: full gate logs, release hash/version, PR URL/base, and
  stack view.
  Conformance check: every trace link and upstream assumption reconciled.
  Recovery: commits remain independently revertible; the lower PRs are
  unaffected.
  Remaining gaps: none.
  Compatibility decision: none beyond the archive compatibility in EP-M2.

## Concrete steps

All commands run from `/home/leynos/Projects/VTCode`. Focused commands use the
shared Cargo cache and write transcripts under `/tmp`:

```bash
cargo nextest run -p vtcode-acp -E 'test(tool_recovery) | test(prompt)' \
  2>&1 | tee /tmp/test-acp-recovery-vtcode-fix-acp-unresolved-tool-recovery.out
cargo nextest run -p vtcode-core -E 'test(write_stdin) | test(exec_session)' \
  2>&1 | tee /tmp/test-stale-exec-vtcode-fix-acp-unresolved-tool-recovery.out
```

At each commit boundary, the scrutineer will run the repository-prescribed
sequential gates and report log paths. After each implementation commit:

```bash
cargo build --release --locked --bin vtcode \
  2>&1 | tee /tmp/build-release-vtcode-fix-acp-unresolved-tool-recovery.out
cargo install --path . --locked --force \
  2>&1 | tee /tmp/install-vtcode-fix-acp-unresolved-tool-recovery.out
```

The final stack publication will push the branch, create a draft PR whose base
is `feat/model-visible-file-versions-noop-patch-guard`, and verify the PR base,
draft state, head SHA, and installed binary hash.

## Validation and acceptance

Red-Green-Refactor evidence will be appended after each milestone. The minimum
acceptance set is:

- a deterministic red failure before ACP recovery implementation;
- passing unit and property tests for closure, ordering, preservation, and
  idempotence;
- an ACP SDK duplex test that observes an incomplete durable result during a
  blocked tool and a real result after completion;
- a durable load/resume test proving no recovered mutating call executes;
- an overlapping prompt test proving no prompt-scoped work runs before
  admission;
- structured stale-session tests proving accurate retry, partial-state,
  circuit-breaker, and next-action fields;
- sequential focused and full repository gates;
- an installed user-level release binary built from the final branch head; and
- a draft stacked PR with the correct base.

No performance regression is expected because repair is linear in message and
tool-call count and runs only during archive attachment. Property tests will
bound generated histories, and production repair will avoid quadratic global
search by processing one tool-call group at a time.

## Idempotence and recovery

Legacy repair is explicitly idempotent. Repeated load/resume cannot add more
recovery results. Runtime replacement identifies results by tool-call ID and
falls back to appending only if the placeholder is absent, so a graceful error
path remains provider-valid. No recovery path executes a historical call.

If a focused test fails after an implementation change, inspect its recorded
log and revert only the current milestone commit if necessary. Do not delete or
hand-edit user archives. The final branch can be dropped without affecting
PR #12 or PR #11.

## Artefacts and notes

The motivating evidence is in these user-owned archives:

```plaintext
~/.config/vtcode/sessions/vtcode-zed-session-2d4c6230-eb4a-4e68-91d6-4480ce64a2e1.json
~/.config/vtcode/sessions/vtcode-zed-session-f846c51e-281b-44d6-b924-3d8457a8ef41.json
```

In the first archive, apply-patch calls
`chatcmpl-tool-8d9bf52719b9a7e6` and
`chatcmpl-tool-a1d7be15e92a3af2` have no Tool-role message. Ordinary successful
calls do have paired Tool results and modified files. Both archives also show a
later `write_stdin` call using an exec-session ID whose originating command had
already completed or whose terminal result was never archived.

## Interfaces and dependencies

The implementation should add private ACP recovery functions with these
conceptual contracts; exact private names may change during red-green-refactor:

```rust
fn stage_tool_calls(messages: &mut Vec<Message>, assistant: Message) -> Vec<String>;
fn replace_tool_results(messages: &mut Vec<Message>, results: &[ToolCallResult]);
fn repair_unresolved_tool_calls(messages: &mut Vec<Message>) -> RecoveryReport;
```

`RecoveryReport` is a small named type carrying at least the repaired call
count, so load code can decide whether to checkpoint and logs can expose what
happened. Recovery content is stable structured JSON rendered as a Tool
message; it includes status `incomplete`, `replayed: false`, state uncertainty,
the original tool name when known, and an explicit next action to inspect the
workspace and submit a materially new tool call if required.

The core implementation should add a typed crate-private exec-session lookup
error and preserve its type through `anyhow` so
`ToolExecutionError::from_anyhow` can map it without parsing text. No new crate
or wire schema is required.

## Revision note

2026-08-16: Initial plan created after publishing PR #12 and inspecting the
current ACP handler, archive attachment, and exec-session error paths. The plan
turns the already falsified false-success hypothesis into write-ahead recovery,
legacy migration, prompt-seriality, and typed stale-session milestones.
