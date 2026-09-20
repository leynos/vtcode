# Terminalize planner setup failures and preserve accepted child tasks

This ExecPlan is a living document. Keep its **Progress**, **Surprises & Discoveries**, **Decision Log**, **Risks**, **Verification Plan**, **Conformance Basis**, and **Outcomes & Retrospective** sections current as evidence changes.

Status: ACTIVE

## Purpose / big picture

A delegated ACP child that fails during planner setup must leave a complete, durable terminal lifecycle and an archive that identifies the task it accepted. At baseline, request/parse failure records only `TurnFailed`; artifact-write failures can return without a terminal event; and prompt/catalog setup can fail before the accepted task reaches the child snapshot. After this work, every planner setup failure will record exactly one `TurnFailed` and one canonical `ThreadCompleted(ErrorDuringExecution)`, close the active turn, drain session persistence, and preserve the accepted task for child archival.

The observable outcome is that the planner-recovery ACP path reports a bounded error without a stuck in-flight turn, does not replay a provider request or tool call, and retains enough task history for a parent to inspect the failed child.

## Constraints

- Start from exact reviewed PR #53 head `98f4e0d2ee25d783185b1075fb024b269b232e58`.
- Change only the planner setup terminalisation and task-publication scope: `crates/codegen/vtcode-core/src/core/agent/runner/task_setup.rs`, `orchestration.rs` only when needed to centralise the terminal owner, focused harness tests, and this plan.
- Preserve `vtcode-exec-events::ThreadEvent` as the only event contract. Add no new event type, public API, dependency, wire shape, durable-store format, retry, or replay.
- Keep diagnostics sanitized and preserve the distinction between provider/request and parse/schema errors.
- Do not change delegated tracker ownership. The inherited workspace-global tracker concern remains [Issue #116](https://github.com/leynos/vtcode/issues/116).
- Do not call paid/live providers. Tests use existing queued providers and local filesystem fixtures only.
- Do not add sleeps, global environment mutation, host-permission races, or artificial timeouts to force an error.
- The shared `acp_serial_gates` scrutineer exclusively runs all tests, formatting, linting, and repository gates. This journeyman will not run them.
- Do not commit before the shared gates are green. Do not push, comment, request review, rebase, or merge in this plateau.

## Tolerances

Stop and escalate to the stack lead if any of these become necessary:

- More than three production source files, more than two focused test files, or more than one plan document.
- Any public Rust API, ACP wire contract, persisted event/file schema, or new dependency.
- A second error-terminalisation path rather than one explicitly owned path, or any duplicate `TurnFailed` event.
- A test seam that requires a production-only test hook, a global environment change, or a permission-dependent filesystem failure.
- A source change that affects tracker ownership, provider retry policy, tool admission, or execution replay.
- A focussed test failure that cannot be explained by the changed lifecycle after three repair attempts.

## Conformance basis

- **PR53-RECOVERY-001:** PR #53 promised durable child failures, safe structured planner diagnostics, and retained accepted task history.
- **ISSUE-21-TERMINAL-001:** [Issue #21](https://github.com/leynos/vtcode/issues/21) requires typed/safe planner diagnostics, distinct termination causes, no duplicate children, no uncertain tool replay, and durable ACP-visible terminal status.
- **CODEX-3952425077:** planner request/parse and artifact-stage failures need one terminal lifecycle, including `ThreadCompleted(ErrorDuringExecution)`.
- **CODEX-3952425085:** the accepted task must be published before a prompt/catalog failure can occur.
- **EVENT-CONTRACT-001:** `vtcode-exec-events::ThreadEvent` is authoritative.

Traceability:

```plaintext
PR53-RECOVERY-001 + ISSUE-21-TERMINAL-001
  -> CODEX-3952425077 -> EP-M1 -> harness::planner_failures_terminalize_once
  -> CODEX-3952425085 -> EP-M1 -> harness::prompt_setup_failure_retains_accepted_task
  -> EVENT-CONTRACT-001 -> EP-M2 -> persisted lifecycle assertions
```

## Context and orientation

`AgentRunner::prepare_task_execution` in `task_setup.rs` creates the session event recorder, validates prompt/catalog state, creates the task conversation, invokes the optional planner, and returns the state consumed by `execute_task`. `run_planner_phase` in `orchestration.rs` requests the planner response and writes the spec, contract, tracker, and feature-list artifacts.

`ExecEventRecorder::thread_failed` records the canonical pair: `TurnFailed` followed by `ThreadCompleted(ErrorDuringExecution)`. `turn_failed` alone closes the turn but does not emit the thread-completion projection. Session-store closure is the authoritative persistence drain.

`runner/tests/harness.rs` contains the existing malformed planner and provider-failure tests. Its existing queued providers are the only provider seam permitted for this work.

## Verification plan

### Invariants

- **INV-TERMINAL-ONCE:** Every error returned from the planner setup phase produces exactly one `TurnFailed` and exactly one `ThreadCompleted` with `ErrorDuringExecution`.
- **INV-TURN-CLOSED:** After a setup failure, the thread handle admits a later `begin_turn`; no active turn remains.
- **INV-DURABLE:** The canonical session event store contains the same terminal events before the failure is returned.
- **INV-TASK-PUBLISHED:** Before the first fallible prompt/catalog operation, the thread snapshot includes the accepted task conversation; failure archival therefore retains the delegated task.
- **INV-NO-REPLAY:** A planner setup failure makes no extra provider request and no tool invocation.
- **INV-DIAGNOSTIC-PRIVACY:** Existing structured error classifications and redaction remain unchanged.

### Lemmas and evidence

| Identifier | Method | Planned evidence | Discharge condition |
| --- | --- | --- | --- |
| INV-TERMINAL-ONCE | Focused harness examples for malformed response, empty provider response, and one post-response artifact write failure | Exact event counts and completion subtype | Each case has one pair only |
| INV-TURN-CLOSED | Existing `ThreadRuntimeHandle` follow-up admission assertion | `begin_turn` succeeds after failure | No in-flight guard remains |
| INV-DURABLE | Read persisted session JSONL in the focused tests | Event variants match in-memory events | Store drain precedes returned error |
| INV-TASK-PUBLISHED | Deterministic prompt-bundle failure test | Snapshot/archived messages include accepted user task | Failure happens after publication |
| INV-NO-REPLAY | Queued provider call accounting and absence of tool events | No unconsumed/second request and no tool event | Error exits before execution |
| INV-DIAGNOSTIC-PRIVACY | Existing malformed/provider assertions plus new test messages | No raw planner content | Existing sanitized fields remain |

The non-trivial runtime axiom is that `SessionStoreSinkHandle::close` resolves only after the event sink has drained. Existing repository tests already exercise this contract; this change will use that mechanism rather than introduce concurrent persistence.

### Gate ownership

The red cases are added before their implementation changes. The root-approved plateau forbids a separate local red gate; the shared scrutineer will run the focused suite after the coupled production change is complete, then run sequential repository gates. This is a workflow exception, not a relaxation of the behavioural assertions.

The requested focused gate packet is:

```bash
cargo nextest run --locked -p vtcode-core -E 'test(malformed_planner_response_records_safe_terminal_turn_failure_without_tools) | test(planner_provider_failure_remains_distinct_from_parse_failure) | test(planner_artifact_failure_terminalizes_once) | test(prompt_bundle_failure_retains_accepted_task)'
```

The exact selector may be updated only if existing naming conventions require it. Expected result: all selected tests pass, with no flaky or leaky result. The shared scrutineer then runs the relevant sequential CI/format/lint/build/documentation gates and captures logs under `/tmp`.

## Milestones

### EP-M1: Specify all failed setup paths and task publication

1. Add focused harness coverage for planner request/parse failure, provider failure, one deterministic post-response artifact failure, and prompt-bundle failure.
2. The tests must inspect event counts, `ThreadCompleted(ErrorDuringExecution)`, turn closure, persisted event drain, accepted task snapshot, sanitisation, and absence of tools.
3. Update this plan with the selected deterministic failure seams and the baseline source facts.

Acceptance: each test expresses a concrete failure contract without a sleep, external provider, or global state.

### EP-M2: Centralise terminalisation and publish the task early

1. Construct and publish the accepted task conversation before the first fallible prompt/catalog operation.
2. Give every error from `run_planner_phase` one terminalisation owner in `prepare_task_execution`, or an equivalent single owner with the same event order.
3. Remove the earlier partial `turn_failed` emission if centralisation would otherwise emit duplicate failure events.
4. Preserve successful prompt construction, conversation state, planner artifact creation, and error classification.

Acceptance: the event contract and task snapshot invariants hold across all tested planner setup failures.

### EP-M3: Freeze and hand off verification

1. Review the focused diff for scope, event ordering, error privacy, and accidental tracker/retry changes.
2. Update the living plan with exact changed files and focused test names.
3. Hand the frozen worktree and gate packet to `acp_serial_gates`. Do not commit until it reports success.

Acceptance: the worktree is clean except for this plateau's narrow changes, and the shared scrutineer has an executable gate packet.

## Progress

- [x] 2026-09-08: verified the starting branch is a new worktree at exact PR #53 head `98f4e0d2ee25d783185b1075fb024b269b232e58`.
- [x] 2026-09-08: reviewed Codex threads 3952425077 and 3952425085 against the exact source; both remain valid. Confirmed tracker ownership is excluded and tracked in Issue #116.
- [x] 2026-09-08: stack lead authorised a private, invocation-scoped
  prompt-bundle builder seam. The normal path supplies the existing validated
  builder; the focused fixture supplies one deterministic error without global
  state, a public API, or changed prompt policy.
- [x] 2026-09-08: authored focused harness coverage in
  `runner/tests/harness.rs`: extended malformed-response and provider-request
  cases; added `planner_artifact_failure_terminalizes_once` using the regular
  `.vtcode/tasks` file fixture; and added
  `prompt_bundle_failure_retains_accepted_task` through the approved builder
  seam. Each asserts exact in-memory and persisted terminal events and closed
  turn admission; the planner cases also assert one provider request.
- [x] 2026-09-08: implemented the source plateau in `task_setup.rs` and
  `orchestration.rs`, pending shared verification. Task setup now publishes the
  accepted conversation before bundle construction and owns terminalisation of
  every error returned by `run_planner_phase`; the planner no longer emits an
  earlier partial terminal event.
- [x] 2026-09-19: reconciled the preserved source WIP onto Stack16 L8 6e0f1728a1f231a7f262e0f5c1573e635fcf365a in the isolated successor worktree. The target's current spelling, surrounding harness coverage, and planner/continuation behaviour remain intact; the source's shared terminalisation owner, prompt-builder seam, and four lifecycle cases are now frozen for shared gates.
- [ ] EP-M3: frozen source and shared-gate hand-off.

## Surprises & discoveries

- 2026-09-08: `run_planner_phase` emits `turn_failed` only for planner-request errors. Artifact failures use `?`; task setup drops the recorder and closes the store. This explains why request failures lack `ThreadCompleted` and artifact failures can lack all terminal events.
- 2026-09-08: the PR #53 snapshot update is after `build_validated_runtime_prompt_bundle`, despite its comment saying task publication precedes fallible setup. Prompt/catalog failures can therefore archive bootstrap-only history.
- 2026-09-08: no production or test change has yet been made. A bounded reconnaissance agent is locating existing deterministic failure seams before implementation.
- 2026-09-08: the real artifact-stage seam is deterministic: after runner
  construction, a regular file at `.vtcode/tasks` makes `write_spec` fail at
  `tokio::fs::create_dir_all` without relying on permissions or global state.
  In contrast, `build_validated_runtime_prompt_bundle` has no error-return
  seam available to the harness: its prompt-composition helper currently
  always returns `Ok`, persistent catalog alignment is logged and returned,
  and malformed tool definitions are rejected only in the later turn loop.
  The requested prompt-bundle failure test therefore requires either a small
  private injection seam or earlier validation; neither is silently assumed.
- 2026-09-08: the stack lead approved the private builder seam instead of
  altering prompt-validation policy. It shares the production setup flow, so
  the test verifies publication and terminalisation around the real boundary.

## Decision log

- 2026-09-08: make `prepare_task_execution` the intended terminalisation owner for planner setup errors, subject to confirming the deterministic fixture seams. This prevents divergent event orders and duplicate `TurnFailed` emission.
- 2026-09-08: preserve existing `ThreadEvent` values and session-store drain instead of a new child-failure transport.
- 2026-09-08: exclude global tracker ownership and leave its pre-existing CodeRabbit disposition intact under Issue #116.
- 2026-09-08: do not run local gates. The shared scrutineer will execute the focused and repository checks after source freeze.
- 2026-09-08: paused the prompt-bundle-failure test design for stack-lead
  direction rather than adding a test-only production hook or treating a
  successful setup assertion as failure-path evidence.
- 2026-09-19: resolve the successor harness overlap by updating the existing provider-failure case in place and adding the artifact and prompt-bundle cases. This preserves L8 test context while retaining the source failure-contract evidence.
- 2026-09-08: the stack lead selected a small private dependency-injection
  seam around the existing prompt-bundle construction. Production supplies the
  same validated builder; tests may supply one failing builder for one
  invocation. No global flags, public API, protocol change, or validation
  policy change is permitted.

## Risks

- **Duplicate terminal events:** moving terminalisation outward could leave `run_planner_phase`'s existing `turn_failed` in place. Mitigation: assert exact event counts in request/parse and artifact tests.
- **Snapshot drift:** moving task publication early could make successful session messages differ. Mitigation: preserve the existing conversation construction and retain success-path tests.
- **Artificial fixture:** the prompt path has no natural deterministic error
  seam. Mitigation: the stack-lead-approved private builder closure exercises
  the actual setup flow while the separate artifact case uses the real
  filesystem failure boundary.
- **Persistence ordering:** dropping the recorder before terminalising would lose the authoritative sink sender. Mitigation: terminalise first, then close the store through the existing helper.
- **Scope drift:** tracker ownership would solve a distinct defect but is already Issue #116. Mitigation: do not edit continuation logic.

## Outcomes & retrospective

The successor plateau is frozen on Stack16 L8 but unverified. The shared acp_serial_gates scrutineer must run the focused selector, then its sequential format, lint, test, and repository gate packet before any commit. The original source patch and the successor resolution note are retained in the transfer control record.
