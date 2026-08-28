# Negotiate Lody usage, tasks and subagents over ACP

This ExecPlan (execution plan) is a living document. The sections
`Constraints`, `Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`,
`Decision log`, `Outcomes & retrospective`, `Conformance basis`, and
`Verification plan` must be kept up to date as work proceeds.

Status: BLOCKED AT EP-M2 SECURITY TOLERANCE

## Purpose / big picture

After this change, a Lody client connected to VTCode over the Agent Client
Protocol (ACP) can negotiate exactly the usage, background-task and subagent
features that VTCode implements. Lody will receive provider token usage, render
delegated agents and managed background processes as first-class tasks, and
list, cancel or inspect output for those tasks. VTCode's model-facing task
tracker remains a standard ACP `Plan`; this work does not create a second task
tracker.

The visible success case is an ACP session in which initialization advertises
`agentCapabilities._meta.lody`, a Baseten-backed turn produces a canonical
`_lody/session/usage_update`, and subagent progress appears as standard ACP
`tool_call` and `tool_call_update` messages carrying `_meta.lody.task`. Lody's
management requests return only tasks owned by the requested ACP session.

## Constraints

- Base the branch on `fix/acp-unresolved-tool-recovery` at
  `80bcd6530c58e4f8b03d5d53e99781024d6f4f58`, the head of PR #13.
- Preserve ACP 1.x interoperability. Lody-specific data must live under the
  `_meta.lody` namespace or `_lody/*` extension methods; other ACP clients must
  continue to work when they ignore those extensions.
- Use `vtcode-exec-events::ThreadEvent` and the existing
  `vtcode_core::subagents::SubagentController`; do not introduce a parallel
  runtime event or task registry.
- Keep VTCode's `task_tracker` output on the standard ACP `Plan` path. Lody's
  `tasks` capability describes background or scheduled work, not plan entries.
- Advertise only implemented operations. Omit `scheduled`, and omit any
  `subagents` management flag whose request handler is not present and tested.
- Do not send `stream_options.include_usage` to every OpenAI-compatible proxy.
  Some proxies reject it. Custom-provider streaming usage must be an explicit
  profile capability and default off.
- Treat every `_lody/subagents/*` request as session-scoped. A task from a
  different ACP session must be indistinguishable from an unknown task.
- Do not add an external Rust dependency. Use the pinned ACP SDK, Serde, the
  existing subagent controller and existing provider response types.
- Keep changes compatible with Rust 1.88 and the repository's formatting,
  Clippy, Whitaker and nextest policies.
- Follow Red-Green-Refactor for every runtime milestone. Commit and gate each
  coherent change, and install the user-level binary after each code commit.

## Tolerances (exception triggers)

- Scope: stop if the implementation needs more than 16 repository files or
  1,200 net lines outside tests, documentation and this plan.
- Interface: stop if satisfying the Lody contract requires changing a released
  public Rust API outside the pre-1.0 ACP/config integration surface.
- Dependencies: stop if a new external crate or JavaScript package is needed.
- Protocol: stop if the current Lody contract cannot be implemented without
  advertising a capability before its handler is available.
- Security: stop if task ownership cannot be established from the existing
  controller state without exposing cross-session task data.
- Iterations: stop after three unsuccessful fixes for the same focused test
  failure and record the competing hypotheses.
- Ambiguity: stop if Lody's published extension contract and its checked-out
  client disagree in a way that changes wire compatibility.

## Risks

- Risk: the ACP SDK routes all unknown extension requests through one
  `ExtRequest` handler, so careless dispatch could consume unrelated methods.
  Severity: high. Likelihood: medium. Mitigation: dispatch only exact `_lody`
  method constants and return a protocol method-not-found error otherwise.
- Risk: one subagent controller may retain tasks from more than one ACP session.
  Severity: high. Likelihood: medium. Mitigation: compute the requested
  session's task closure from parent identifiers and test generated mixed-session
  forests; validate ownership before list, cancel and output.
- Risk: `SubagentStatus::Closed` and background `Stopped` do not encode every
  Lody terminal reason. Severity: medium. Likelihood: high. Mitigation: define
  one explicit, documented mapping; use `failed` for a closed child and
  `completed` for a normally stopped background process, preserving errors and
  raw summaries where the Lody schema permits them.
- Risk: provider usage may be absent or incomplete. Severity: medium.
  Likelihood: medium. Mitigation: emit no usage notification when a provider
  supplies none, never invent token counts, and represent unavailable optional
  fields by omission.
- Risk: Baseten streaming usage is currently absent because VTCode does not
  request it for custom providers. Severity: high for the user's providers.
  Likelihood: certain. Mitigation: add an opt-in `supports_stream_usage`
  profile capability, exercise a Baseten-shaped final usage chunk with
  vidai-mock, and leave the default off.
- Risk: lifecycle events can arrive after a session ends. Severity: medium.
  Likelihood: low. Mitigation: retain the existing per-session forwarder
  lifetime, parent-session filter and abort-on-drop behaviour.

## Progress

- [x] (2026-08-28 11:08Z) Create
  `fix/acp-lody-task-lifecycle-negotiation` from PR #13 in an independent
  worktree.
- [x] (2026-08-28 11:20Z) Verify the VTCode event/controller paths and the
  current Lody extension contract.
- [x] (2026-08-28 11:28Z) Confirm Baseten's streamed-usage contract and the
  current custom-provider opt-in gap.
- [x] (2026-08-28 12:39Z) Obtain approval for this ExecPlan and begin EP-M1.
- [x] (2026-08-28 13:28Z) Complete EP-M1 red and focused green tests for
  standard task lifecycle updates, conditional negotiation and stable IDs.
- [x] (2026-08-28 15:24Z) EP-M1: replace `_vtcode/taskLifecycle` with standard
  task-carrying ACP tool updates and negotiate subagent lifecycle.
- [ ] (blocked 2026-08-28 15:24Z) Resolve the EP-M2 security tolerance:
  persisted background records do not carry an explicit owning ACP session,
  so foreign-task isolation cannot be proved without expanding the core model.
- [ ] EP-M2: implement and negotiate background tasks plus session-scoped
  subagent list, cancel and output methods.
- [ ] EP-M3: emit negotiated provider usage and opt custom providers into
  streaming usage.
- [ ] EP-M4: update documentation, run release gates, install the binary, push
  the stack and open the draft PR.

## Surprises & discoveries

- Observation: Lody's version-1 task schema accepts only `pending`,
  `in_progress`, `completed` and `failed`; it does not accept `killed`.
  Evidence: `packages/shared/src/acp/claude-subagent-task.ts` in the checked-out
  Lody source defines the closed status enum used by `_meta.lody.task`.
  Impact: a closed child maps to `failed`, with its error retained, rather than
  the unsupported `killed` value in the draft risk mitigation.
- Observation: the EP-M1 red run failed at compile time because
  `lody_task_session_update` was intentionally absent.
  Evidence: `/tmp/red-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm1.out`
  reports `E0425` from the new task lifecycle unit test.
  Impact: the red test directly proves that the new standard ACP adapter is
  required; the existing private notification cannot satisfy it.
- Observation: EP-M1 passed the complete repository release gate and was
  committed as `16ba7b516`.
  Evidence: `/tmp/check-VTCode-fix-acp-lody-task-lifecycle-negotiation-2.out`
  records rustfmt, policy checks, Clippy, build, 6,582 tests, harness
  regressions and rustdoc passing.
  Impact: the standard lifecycle repair is a complete, independently
  releasable plateau.
- Observation: persisted background records are loaded into every controller
  for a workspace without an explicit owning ACP session, while background
  cancellation itself performs no ACP ownership check.
  Evidence: `SubagentController::new` loads `background_children` from
  workspace state; `BackgroundSubprocessEntry` contains generated runtime and
  exec session IDs but no parent ACP session ID.
  Impact: EP-M2 cannot prove `SESSION-ISOLATION` for background list, cancel or
  output using the existing controller state. This activates the plan's
  security tolerance and requires a deliberate persisted-model/API expansion
  or removal of background management from this stack.
- Observation: the current public background snapshot API hard-limits previews
  to 24 lines, while the approved Lody output contract specifies a caller
  default of 200 and a hard maximum of 10,000.
  Evidence: `SUBAGENT_PREVIEW_LINES` and `background_snapshot()` clamp both
  live and archived output before the ACP adapter can apply a requested tail.
  Impact: meeting the output contract also requires extending the controller
  API rather than adding only an ACP adapter.
- Observation: ACP SDK `ExtRequest` is a schema value, not a directly
  registerable typed request handler.
  Evidence: `ClientRequest::ExtMethodRequest` owns extension dispatch and the
  SDK strips the leading underscore before constructing `ExtRequest`.
  Impact: EP-M2 must use a custom untyped dispatcher or a carefully scoped
  `ClientRequest` catch-all that returns `Handled::No` for standard requests.

- Observation: VTCode already emits the model's task tracker as a standard ACP
  `Plan`, including blocked-task labels and durable replay.
  Evidence: `crates/codegen/vtcode-acp/src/zed/agent/task_progress.rs` and its
  duplex tests in `zed/agent/handlers.rs`.
  Impact: no new plan/task-tracker protocol is required. The Lody `tasks`
  capability will advertise only managed background work.
- Observation: `_vtcode/taskLifecycle` is neither a standard ACP task update nor
  a Lody-recognized legacy extension, and its discriminator is incompatible
  with Lody's Claude compatibility parser.
  Evidence: `crates/codegen/vtcode-acp/src/zed/agent/task_lifecycle.rs` uses
  `message.type`; Lody commit `dd241fd4108ba5de2f7e2d8d627713152d812a06`
  recognizes only `_claude/taskLifecycle` and `_kimi/taskLifecycle` legacy
  carriers using a different shape.
  Impact: the custom notification must be removed rather than renamed.
- Observation: the existing `SubagentController` already exposes status,
  close/cancel, child snapshots and background output previews.
  Evidence: `vtcode-core/src/subagents/mod.rs`,
  `controller_spawn_run.rs` and `controller_background_ops.rs`.
  Impact: Lody management is an ACP adapter concern, not a new core subsystem.
- Observation: Baseten documents per-request token usage, cached input tokens
  and final streamed usage, but the final usage chunk is conditional on
  `stream_options.include_usage: true`.
  Evidence: Baseten's Chat Completions reference and GLM-5.3 Flash model page;
  VTCode currently adds the option only for native OpenAI in
  `vtcode-llm/src/providers/openai/provider/streaming.rs`.
  Impact: negotiated Lody usage would remain empty for the user's Baseten
  sessions without a provider-profile opt-in.
- Observation: Baseten's aggregate `/v1/model_apis/usage` endpoint is bucketed
  across requests and API keys.
  Evidence: Baseten's model API pricing and limits documentation.
  Impact: it is unsuitable for per-turn ACP usage and is out of scope.

## Decision log

- Decision: stack this work directly on PR #13 rather than the unpublished
  skill-discovery or context-limit branches in the original worktree.
  Rationale: the user requested a new stacked PR, and PR #13 is the latest
  published stack layer. This avoids silently coupling the integration to
  unrelated unpublished commits.
  Date/Author: 2026-08-28 / Codex.
- Decision: use standard ACP `ToolCall`/`ToolCallUpdate` messages with stable
  `task:<task-id>` identifiers and `_meta.lody.task` snapshots.
  Rationale: this is the Lody contract and remains meaningful to non-Lody ACP
  clients, unlike a provider-specific lifecycle notification.
  Date/Author: 2026-08-28 / Codex.
- Decision: negotiate `usage: {version: 1}` unconditionally, `tasks:
  {version: 1, background: true}` only when managed background work is enabled,
  and `subagents` only when a controller exists. Advertise list, cancel and
  output only after their handlers land in the same coherent commit.
  Rationale: capabilities are promises, not product marketing.
  Date/Author: 2026-08-28 / Codex.
- Decision: emit usage as per-response deltas, not cumulative session totals.
  Rationale: Lody's usage tracking service sums incoming updates per ACP
  session; cumulative snapshots would double-count.
  Date/Author: 2026-08-28 / Codex.
- Decision: add `supports_stream_usage` to custom-provider profiles rather than
  special-casing Baseten hostnames.
  Rationale: the capability is part of the OpenAI-compatible peer contract,
  while hostname inference is brittle and would expose other proxies to known
  400 responses.
  Date/Author: 2026-08-28 / Codex.

## Outcomes & retrospective

EP-M1 landed as an independently gated lifecycle repair. Standard ACP clients
now receive task-shaped tool calls, Lody receives `_meta.lody.task`, and
capability negotiation is truthful. EP-M2 is paused at the explicit security
tolerance because the existing persisted background model cannot establish
ACP-session ownership. No management handler or capability has been exposed
prematurely. On resolution, this section will compare the management and usage
results against the remaining purpose above.

## Context and orientation

VTCode's ACP server lives in `crates/codegen/vtcode-acp`. The SACP handler
registration and initialization response are in
`src/zed/agent/handlers.rs`; standard session notifications are sent through
`src/zed/agent/updates.rs` and `src/zed/connection.rs`. Per-session runtime
state is in `src/zed/types.rs`.

`src/zed/agent/task_lifecycle.rs` subscribes to
`vtcode_core::subagents::SubagentProgressEvent`, filters events by parent ACP
session and currently emits the ignored `_vtcode/taskLifecycle` notification.
The controller that owns those events is in
`crates/codegen/vtcode-core/src/subagents`. It already provides status lists,
child shutdown, child transcript snapshots, background shutdown and background
output previews.

Lody extensions are negotiated in `agentCapabilities._meta.lody`. Version 1
defines `usage`, `tasks` and `subagents` independently. Session usage travels
through `_lody/session/usage_update`. Management uses
`_lody/subagents/list`, `_lody/subagents/cancel` and
`_lody/subagents/output`. Lifecycle itself does not use a custom method: it is
a standard ACP tool call whose `_meta.lody.task` value is a complete task
snapshot.

The word "task" has three distinct meanings here. VTCode's model-facing
`task_tracker` is an ACP `Plan`. Lody's `tasks` capability says whether the
agent supports managed background or scheduled work. Lody task metadata is the
UI/history representation for subagents, background work or scheduled work.
Only the latter two are changed by this plan.

Provider responses carry `vtcode_commons::llm::Usage` with prompt, completion,
cache-read and cache-creation counts. OpenAI-compatible streams expose usage in
a terminal SSE chunk only when requested. The custom-provider OpenAI router
already parses a received usage chunk, but it does not request one from
Baseten today.

## Conformance basis

- User requirement, 2026-08-28: implement negotiated Lody usage, tasks and
  subagents, beginning with the standard task-lifecycle repair.
- ACP SDK `agent-client-protocol = 2.0.0`, schema 1.5.0, and ACP v1
  extensibility/tool-call contracts pinned by this repository.
- Lody extension contract `LodyAI/acp-extension-core` main revision
  `23c792b910a903b74601e346473827106f991715` (`capabilities.ts`, `methods.ts`,
  `session.ts`, `usage.ts`).
- Lody client/parser revision
  `dd241fd4108ba5de2f7e2d8d627713152d812a06` in
  `/home/leynos/Projects/Lody`.
- Baseten Chat Completions, model API pricing/limits, observability and GLM-5.3
  Flash documentation as observed on 2026-08-28.
- No separate VTCode technical design or ADR governs Lody extensions. This
  ExecPlan is the lightweight architecture contract for this branch.

Trace links:

```plaintext
USER-LIFECYCLE -> EP-M1 -> acp_duplex_emits_lody_task_updates
USER-SUBAGENTS -> EP-M1 -> EP-M2 -> acp_duplex_manages_only_session_tasks
USER-TASKS -> EP-M2 -> lody_background_capability_is_truthful
USER-USAGE -> EP-M3 -> baseten_usage_reaches_lody
```

## Verification plan

- Obligation: `NEGOTIATION-TRUTH`. Every advertised Lody feature has a working
  handler or emission path, disabled subagent/background features are omitted,
  and no unimplemented scheduled support is advertised.
  Method: parameterized unit tests plus official ACP duplex behavioural tests.
  Rationale: the state space is the finite cross-product of controller absent,
  controller present and background disabled/enabled.
  Domain: all capability combinations VTCode can construct.
  Artefact: tests beside `advertised_agent_capabilities` and in the ACP duplex
  harness in `zed/agent/handlers.rs`.
  Evidence: the red test observes missing `_meta.lody`; the green test parses
  exact versioned capabilities and successfully invokes every advertised
  method.
  Non-vacuity: include witnesses for no controller, child-only controller and
  child-plus-background controller; a negative control advertises no
  `scheduled` key and rejects an unknown `_lody` method.
- Obligation: `TASK-IDENTITY`. Every lifecycle update for one VTCode task uses
  one stable `task:<id>` tool-call identifier, carries a complete valid
  `_meta.lody.task` snapshot and maps terminal state monotonically.
  Method: table tests for the finite status enums and a property test over
  generated valid lifecycle sequences.
  Rationale: examples prove wire shape while generated sequences catch
  accidental identifier changes, terminal regressions and incomplete metadata.
  Domain: child and background statuses, summaries/errors and timestamp
  presence across sequences of length 1 through 32.
  Artefact: `task_lifecycle.rs` unit/property tests and official ACP duplex
  lifecycle test.
  Evidence: the existing custom-notification assertion fails after the red
  expectation is changed; green observes an initial `ToolCall`, subsequent
  `ToolCallUpdate` values and no `_vtcode/taskLifecycle` notification.
  Non-vacuity: classify child/background and pending/running/terminal cases;
  include a seeded ID-changing mapper that the stable-ID property rejects.
- Obligation: `SESSION-ISOLATION`. List, cancel and output never reveal or
  mutate a task outside the requested ACP session, including nested children.
  Method: proptest-generated task forests plus behavioural requests over the
  duplex ACP connection.
  Rationale: ancestry and mixed-session arrangements are combinatorial and
  deserve wider coverage than hand-picked examples.
  Domain: acyclic forests up to 32 tasks across two to four session roots,
  with foreground and background nodes and every status.
  Artefact: pure ownership-selection property tests and handler-level ACP
  tests.
  Evidence: owned nested tasks are listed and manageable; foreign IDs return
  the same not-found response as unknown IDs and remain unchanged.
  Non-vacuity: require generators to produce direct, nested, foreign and
  background tasks; a negative control that filters only direct children must
  fail on a generated nested witness.
- Obligation: `USAGE-DELTA`. Each provider usage sample produces exactly one
  non-negative Lody delta with the same input, output, cache-read and
  cache-creation counts, keyed by the response model; absence produces no
  notification.
  Method: property tests for numeric conversion and ACP behavioural tests.
  Rationale: generated boundaries exercise zero, ordinary and maximum `u32`
  values without relying on live billing.
  Domain: every field across `0..=u32::MAX`, provider usage present/absent and
  model identifiers including Unicode.
  Artefact: Lody usage-mapping tests and a duplex notification test.
  Evidence: red observes no usage notification; green parses a canonical
  `_lody/session/usage_update` whose per-model and aggregate deltas agree.
  Non-vacuity: classify zero/non-zero/cache/no-cache samples; a swapped
  prompt/completion negative control must fail.
- Obligation: `BASETEN-STREAM-USAGE`. A custom OpenAI-chat provider requests
  usage only when its resolved profile opts in, and a Baseten-shaped terminal
  usage chunk reaches the ACP Lody notification unchanged.
  Method: request-shape unit tests, wiremock-style deterministic provider
  playback and a vidai-mock physics test with streamed SSE chunks.
  Rationale: unit tests prove configuration, playback proves parser contract,
  and vidai-mock proves the assembled streaming path without external cost or
  nondeterministic network timing.
  Domain: provider default/profile override precedence, opt-in/out, content
  chunks followed by terminal usage, and usage absent despite opt-in.
  Artefact: custom-provider config/provider tests and the existing ACP provider
  fixture environment extended with a Baseten usage scenario.
  Evidence: request capture contains `stream_options.include_usage: true` only
  for the opted-in profile; Lody receives one delta after the terminal chunk.
  Non-vacuity: the opt-out request is asserted to omit `stream_options`; a
  fixture with no terminal usage must produce no notification.

External axioms are limited to ACP's documented extension routing and Lody
Core revision `23c792b9` accepting the stated version-1 shapes. Baseten's
documented final usage chunk is treated as a peer contract; the repository-owned
request construction and parsing are still exercised against faithful local
boundaries. No formal prover or model checker is proportionate: the introduced
invariants are finite serialization, ownership filtering and monotone event
mapping without concurrency shared-memory semantics beyond the existing
broadcast channel.

## Plan of work

EP-M1 adds a private `zed/agent/lody.rs` module containing the versioned
subagent-lifecycle capability and task-metadata DTOs, then changes
`task_lifecycle.rs` to retain its broadcast subscription, session filter and
lifetime while replacing the custom extension notification. The forwarder will
remember which stable task IDs it has emitted:
the first snapshot becomes `SessionUpdate::ToolCall`, and later snapshots
become `SessionUpdate::ToolCallUpdate`. Both carry `_meta.lody.task` and a
normal ACP status/title. The obsolete method and legacy-shaped message builders
will be deleted. `handle_initialize` will advertise only
`subagents: {version: 1, lifecycle: true}` when an actual controller exists.

EP-M2 extends the private Lody module with typed management DTOs and registers
one `ExtRequest` dispatcher in `handlers::install_handlers`.
Exact `_lody/subagents/list`, `/cancel` and `/output` methods will parse typed
parameters, resolve the requested `SessionHandle` and controller, establish
task ownership, then call existing controller operations. List will combine
child and managed-background entries in deterministic ID order. Cancel will
use child `close` or background `force_cancel_background`. Output will render a
bounded tail (default 200 lines, hard maximum 10,000) from the existing child
snapshot or background preview. Unknown or foreign task IDs will share one
not-found response. The same commit will add `list`, `cancel` and `output`
negotiation flags, plus `tasks: {version: 1, background: true}` only when the
controller reports managed background support.

EP-M3 emits a canonical `_lody/session/usage_update` after each provider
response that contains usage, including intermediate tool-loop responses.
`usage` and the matching `modelUsage[model]` entry are per-response deltas.
The custom-provider profile gains `supports_stream_usage`; resolved profile
precedence controls whether custom OpenAI-chat streams include
`stream_options.include_usage`. The stream decoder's existing base token
support is reused. Nested Baseten cached/reasoning detail remains optional:
cached tokens will be included only if already represented reliably in the
normalized `Usage`, and reasoning tokens will not be fabricated.

EP-M4 updates `docs/acp/ACP_INTEGRATION.md`,
`docs/acp/ACP_QUICK_REFERENCE.md`, the custom-provider configuration reference
and any generated schema fixture required by the established config workflow.
It then runs release gates, installs the user binary, updates this plan with
evidence, pushes the branch and opens a draft PR based on
`fix/acp-unresolved-tool-recovery`.

Each milestone is test-first and ends with focused tests, full commit gates, a
commit and a user-level binary installation before the next milestone begins.

## Milestones and plateaus

- Identifier and outcome: EP-M1, standard task lifecycle visible to Lody and
  generic ACP clients with truthful lifecycle-only negotiation.
  Requirements and gaps: discharges USER-LIFECYCLE and advances
  USER-SUBAGENTS.
  Acceptance evidence: lifecycle duplex and property tests pass; custom method
  is absent; initialize advertises lifecycle only when a controller exists.
  Conformance check: existing event/filter/lifetime contracts remain intact,
  and no management handler is advertised prematurely.
  Recovery: revert the lifecycle commit; no persisted migration exists.
  Remaining gaps: management and usage.
  Compatibility decision: no compatibility layer for `_vtcode/taskLifecycle`;
  it had no documented consumer and Lody ignored it.
- Identifier and outcome: EP-M2, session-scoped Lody subagent/background-task
  management and truthful management/task negotiation.
  Requirements and gaps: discharges USER-SUBAGENTS and USER-TASKS.
  Acceptance evidence: list/cancel/output duplex tests and ownership property
  tests pass.
  Conformance check: no cross-session data, management flags match handlers,
  and scheduled support remains omitted.
  Recovery: revert handler registration and remove its advertised flags.
  Remaining gaps: provider usage.
  Compatibility decision: none; these are new versioned extension methods.
- Identifier and outcome: EP-M3, provider usage reaches Lody, including opted-in
  Baseten streams.
  Requirements and gaps: discharges USER-USAGE.
  Acceptance evidence: mapping properties, playback, vidai-mock physics and ACP
  usage notification tests pass.
  Conformance check: proxy-safe default remains off; absent metrics stay absent.
  Recovery: disable the profile flag or revert the usage commit.
  Remaining gaps: documentation/publication only.
  Compatibility decision: the new optional config field defaults off, so
  existing custom-provider request bodies remain byte-for-byte compatible.
- Identifier and outcome: EP-M4, documented, gated, installed and published
  stacked PR.
  Requirements and gaps: closes documentation and delivery requirements.
  Acceptance evidence: release gates, binary version/path, remote SHA, PR base
  and draft state.
  Conformance check: reconcile every discovery and trace link before marking
  COMPLETE.
  Recovery: use `gh stack push` after corrections; never force-push manually.
  Remaining gaps: provider limits, compaction metadata, notices, final-answer
  phase metadata and scheduled tasks remain independent follow-ups.
  Compatibility decision: none.

## Concrete steps

Work from:

```bash
cd /home/leynos/Projects/VTCode.worktrees/lody-task-lifecycle-negotiation
```

At each red/green/refactor stage, capture output under `/tmp` using the action,
project and branch naming convention. Use focused nextest filters during TDD;
never run `cargo test`.

```bash
cargo nextest run -p vtcode-acp -E 'test(lody)' 2>&1 \
  | tee /tmp/test-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
```

Run all full commit gates sequentially through the `scrutineer` subagent. It
must use repository Make/script entry points, shared Cargo caches and `/tmp`
logs. Do not run formatting, linting and tests in parallel.

After every code commit, install and verify the user binary:

```bash
cargo install --path . --locked --force 2>&1 \
  | tee /tmp/install-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
readlink -f /home/leynos/.local/bin/vtcode
/home/leynos/.local/bin/vtcode --version
```

Before publication, inspect and submit the stack non-interactively:

```bash
gh stack link 13 fix/acp-lody-task-lifecycle-negotiation
gh stack submit --auto
gh stack view --json
```

If GitHub stacked PRs reject the link, preserve the branch and create an
ordinary draft PR with base `fix/acp-unresolved-tool-recovery`, then report the
stack-link limitation rather than changing the base.

## Validation and acceptance

Red-Green-Refactor evidence will be recorded in `Artefacts and notes` for every
milestone. The final branch is accepted only when:

- initialization advertises exact, versioned and conditional Lody
  capabilities;
- standard ACP plans from `task_tracker` remain unchanged;
- lifecycle produces stable standard tool calls with valid task metadata and
  never emits `_vtcode/taskLifecycle`;
- list/cancel/output succeed for owned foreground and background tasks and
  reject foreign IDs;
- usage deltas preserve all available normalized counts and absent usage emits
  nothing;
- Baseten-shaped streaming usage is requested only under explicit profile
  opt-in and reaches Lody through local playback/physics tests;
- `./scripts/check.sh` passes through `scrutineer` before every code commit and
  once more at the final branch tip;
- the installed user binary resolves through `/home/leynos/.local/bin/vtcode`;
- the pushed local and remote SHA match; and
- the new draft PR targets `fix/acp-unresolved-tool-recovery` and is linked to
  the stack containing PR #13.

Performance acceptance is qualitative: the lifecycle forwarder remains one
broadcast task per session, usage conversion performs bounded map construction
per provider response, and management list/output work is bounded by the
existing controller task count and requested tail. No network call to Baseten's
aggregate usage API is permitted.

Security acceptance is the `SESSION-ISOLATION` obligation plus existing ACP
permission behaviour remaining green. No management request may bypass the
controller's cancellation semantics or read an arbitrary transcript path.

## Idempotence and recovery

Focused tests, gates and binary installation are idempotent. Extension
notifications are observational and do not change durable session history in
VTCode. List and output are read-only; cancel is intentionally idempotent for a
terminal owned task and never automatically replays work.

If a rebase or stack operation conflicts, stop, inspect both intents and use
`gh stack rebase`/`gh stack push`; do not use `git push --force`. If a test
fixture leaves a mock server running, terminate only the process started by the
current test script, never unrelated Rust or agent processes.

## Artefacts and notes

Initial evidence:

```plaintext
Branch: fix/acp-lody-task-lifecycle-negotiation
Base:   80bcd6530c58e4f8b03d5d53e99781024d6f4f58 (PR #13)
Lody Core: 23c792b910a903b74601e346473827106f991715
Lody client: dd241fd4108ba5de2f7e2d8d627713152d812a06
```

EP-M1 Red-Green-Refactor evidence:

```plaintext
RED:   /tmp/red-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm1.out
       E0425: lody_task_session_update was absent.
GREEN: /tmp/green-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm1.out
       5 passed: status mapping, stable-ID property, official ACP duplex and
       capability present/absent tests.
```

Baseten's documented streamed-usage request is:

```json
{
  "stream": true,
  "stream_options": {
    "include_usage": true
  }
}
```

`continuous_usage_stats` is model-specific prior art, not required for the
terminal delta this plan consumes.

## Interfaces and dependencies

Define private Serde DTOs in `crates/codegen/vtcode-acp/src/zed/agent/lody.rs`
for the version-1 capability map, task metadata, session usage update and three
management request/response shapes. The serialized field names must match Lody
Core revision `23c792b9` exactly. These types are an adapter boundary, not a new
public crate API.

`advertised_agent_capabilities` will accept the agent/controller state needed
to build `acp::AgentCapabilities.meta`. `task_lifecycle` will construct
`acp::ToolCall` and `acp::ToolCallUpdate` values and send them through the
existing standard session-notification connection.

The extension dispatcher will accept `acp::ExtRequest` and respond with
`acp::ExtResponse`. It will call only existing `SubagentController` methods:
`status_entries`, `background_status_entries`, `close`,
`force_cancel_background`, `snapshot_for_thread` and `background_snapshot`.

Add `supports_stream_usage: Option<bool>` to
`vtcode_config::core::CustomProviderProfileConfig` and
`ResolvedCustomProviderProfile`. The custom OpenAI-chat provider will consult
the resolved profile for the request model before adding
`stream_options.include_usage`; native OpenAI keeps its existing behaviour.

No new library dependency or deployed persistent-format migration is planned.

---

Revision note (2026-08-28): created the initial draft after verifying VTCode's
ACP and subagent paths, Lody Core/client contracts and Baseten's streamed-usage
requirements. Implementation remains blocked on explicit plan approval.
