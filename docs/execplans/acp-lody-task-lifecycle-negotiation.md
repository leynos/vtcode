# Negotiate Lody usage, tasks and subagents over ACP

This ExecPlan (execution plan) is a living document. The sections `Constraints`,
`Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`, `Decision log`,
`Outcomes & retrospective`, `Conformance basis`, and `Verification plan` must
be kept up to date as work proceeds.

Status: COMPLETE

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
- Deviation, recorded 2026-10-31 and re-measured after the review repairs and
  the restack onto the current base head: the branch exceeds both tolerances
  against `fix/acp-unresolved-tool-recovery`
  (`51aa49eb66c0243155763d31b79a6cf7a4ce8f4f`). Counting production paths only
  — every path except `docs/`, `*.md`, `proptest-regressions/`, and any path
  segment containing `test` — 32 files changed with 1,509 insertions and 168
  deletions, 1,341 net lines, against tolerances of 16 files and 1,200 lines.
  The file-count overrun and the additional line-tolerance overrun come from
  approved scope, not incidental drift: the EP-M2 security expansion touches
  the config schema, the core controller model, provider profiles and the ACP
  adapter; the first review-repair round added tree-aware routing across those
  same modules; the second added shared queue accounting for the attach-time
  snapshot publisher; and the third made that accounting atomic against the
  forwarder's dequeue and extracted the managed-background ownership predicate.
  The requirement to keep the ownership expansion additive and both repair
  rounds were directed by the user, so the deviation is recorded rather than
  treated as a live stop condition. The documentation share is excluded from
  that count, but the generated configuration reference alone changes 928 added
  and 926 removed lines: a generator run that restored truncated JSON defaults,
  followed by mdtablefix's canonical re-padding of the widened table (see the
  Decision log). A diff of that size is expected for this one generated table
  and is not separate feature work.

## Risks

- Risk: the ACP SDK routes all unknown extension requests through one
  `ExtRequest` handler, so careless dispatch could consume unrelated methods.
  Severity: high. Likelihood: medium. Mitigation: dispatch only exact `_lody`
  method constants and return a protocol method-not-found error otherwise.
- Risk: one subagent controller may retain tasks from more than one ACP session.
  Severity: high. Likelihood: medium. Mitigation: compute the requested
  session's task closure from parent identifiers and test generated
  mixed-session forests; validate ownership before list, cancel and output.
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
- [x] (2026-08-28 16:02Z) Resolve the EP-M2 security tolerance:
  persisted background records do not carry an explicit owning ACP session, so
  foreign-task isolation cannot be proved without expanding the core model. The
  user approved the robust expansion: persist explicit ownership, hide legacy
  ownerless records from ACP management and add a bounded output-tail API
  before registering any management handlers.
- [x] (2026-08-28) EP-M2: implement and negotiate background tasks plus
  session-scoped subagent list, cancel and output methods. The core expansion
  persists optional `owner_session_id`, hides legacy ownerless records from
  ACP, and exposes bounded output tails (default 200, maximum 10,000) while
  preserving the existing 24-line preview API. The exact extension dispatcher
  and real ACP duplex tests cover negotiation, owned list/output/cancel and
  foreign-task indistinguishability from unknown tasks.
- [x] (2026-08-28) EP-M3: emit negotiated provider usage and opt custom
  providers into streaming usage. Per-response usage deltas now use the
  canonical `_lody/session/usage_update` method, and custom-provider streaming
  usage is opt-in through profile resolution.
- [x] (2026-08-28) EP-M4: update documentation, run release gates, install the
  binary, push the stack and open the draft PR.
  - Current ACP/Lody negotiation, task/subagent management and usage are now
    documented alongside the custom-provider streamed-usage opt-in.
  - The config-reference generator completed; its unrelated pre-existing
    full-file drift was excluded and the two new schema rows were retained.
    A later formatting commit (`677c32f15`) re-laid the whole table at the
    formatter's canonical column widths, so the file's diff against the base
    is 922 reformatted lines plus the two-row content change, not 922 lines of
    new content.
  - Both user-level Baseten profiles now opt into streamed usage.
  - The final release gate passed with 6,588 tests and 16 skips; the user-level
    binary was reinstalled and resolves through `/home/leynos/.local/bin/vtcode`.
  - Draft PR [#14](https://github.com/leynos/vtcode/pull/14) targets
    `fix/acp-unresolved-tool-recovery`. PRs #11 through #14 form stack #16,
    preserving the intended four-layer base chain.
- [x] (2026-10-31) EP-M5 review repair: close the three CodeRabbit correctness
  gaps and add the boundary coverage it asked for.
  - Nested tasks are now reachable. `SubagentController` exposes
    `descendant_controllers()` (pointer-deduplicated breadth-first walk over
    `ChildRecord::child_controller`), an `attach_child_controller` seam that
    fails closed on an unknown task, and observer-scope inheritance so a
    child-scoped controller publishes under the root session's observer key.
    `_lody/subagents/{list,cancel,output}` walk the root plus every descendant
    controller and route each operation to the controller that actually owns
    the task, still filtered by the requested session's ownership closure.
  - A settled task no longer changes outcome when it is cancelled. Foreground
    `close` returns the existing entry for any terminal status, and background
    cancellation returns the recorded entry when the process is already
    inactive, so `completed`/`failed`/`closed` and `stopped`/`error` keep their
    status, timestamps and error text.
  - Attaching a lifecycle forwarder to a session that already has active work
    publishes an initial snapshot for every owned active child and background
    task. Without it, a resumed task that never transitions again stays
    invisible to the client.
  - New boundary coverage: `subagents/tests/tree_routing_tests.rs` (descendant
    discovery, deduplication, unknown-task rejection, observer-scope
    inheritance through two levels, and settled-outcome preservation for both
    close and cancel), a projection property test asserting stable tool-call
    IDs and non-regressing terminality over generated lifecycle walks, and a
    duplex assertion that an already-running background task is published when
    the forwarder attaches. Assembled provider-stream usage was already covered
    end to end by `streamed_audit.rs`; the Baseten parser and opt-in paths stay
    covered by `vtcode-llm/tests/vidaimock_streaming.rs`.
- [x] (2026-11-01) EP-M5 second review repair: close the findings from
  Assessment-ID ACP-LODY-EP-20261031-2.
  - Attach-time snapshots now share the live path's queue accounting.
    `LifecycleQueueTracker` owns `is_closed`, `record_enqueued`, an awaited
    `send` and the test-only `depth`, and `publish_initial_task_snapshots`
    enqueues through `queue.send`, so one receiver dequeue can no longer cancel
    another source's tracked depth.
  - The `SESSION-ISOLATION` property now covers its stated domain: acyclic
    mixed forests of 3 to 32 tasks across two to four session roots, with
    foreground and background nodes, every status, shuffled delivery order and
    a direct-child-only negative control rejected by a generated nested
    witness.
  - The nested duplex test cancels an owned grandchild directly while its
    sibling and parent stay running, then confirms that closing the child
    settles the remaining subtree. It also compares the complete foreign and
    unknown error values for both output and cancel.
  - The assembled Baseten usage tests now carry the `USAGE-DELTA` assembled
    evidence, replacing the older split provenance in the EP-M5 addendum.
  - Tolerances record the measured scope for this head (32 production paths,
    1,496 insertions, 168 deletions, 1,328 net) and state that both the file
    and line tolerances are exceeded under directed scope.
- [x] (2026-11-01) EP-M5 third review repair: close the findings from
  Assessment-ID ACP-LODY-EP-20261101-3.
  - Channel publication and its accounting now share one synchronous critical
    section. `try_send` counts while holding the state lock across
    `Sender::try_send`, and the awaited path reserves capacity first, then
    checks closure, counts and publishes through the permit under the same
    lock, so the synchronous lock is never held across an await.
  - `the_forwarder_never_observes_an_unaccounted_item` is a concurrent
    invariant regression, not a deterministic reproduction of the removed
    window. Over 256 rounds on a multi-threaded runtime it asserts the
    receiver observes depth one at its own observation point before
    dequeueing, and that an empty channel leaves tracked depth, the metric
    depth and the drain count at zero. A scheduler that never selects the
    producer's former publish/count interval cannot make this test fail a
    faulty implementation.
  - `the_pre_repair_publish_then_count_order_fails_the_invariant` supplies the
    deterministic negative control the concurrency test cannot: it reproduces
    the old order literally against the same tracker (publish, receive,
    dequeue, then count) with no scheduler involvement, and shows the
    observation point reports zero, the dequeue is lost and the channel is
    empty while depth stays one. The repaired production paths cannot reach
    that state because they hold the state lock from before publication
    through the count.
  - The managed-background ownership predicate is now shared:
    `lody::owned_background_ids` selects by `owner_session_id` for both list
    and cancel, and
    `lody_tests.rs::background_ownership_is_the_owner_session_field` proves it
    over generated owned, foreign and ownerless records in shuffled order with
    an owner-agnostic negative control.
  - The forest property is documented as foreground-controller ancestry that
    includes the entry's `background` flag, and node 0 is forced background so
    every case carries a background witness. Managed background records are
    covered separately by the owner-session property.
  - Tolerances re-measured for this head: 32 production paths, 1,509
    insertions, 168 deletions, 1,341 net.
  - Committed as `60b2c4cc5ea8c39ce15e4f844888106af6af0233` and pushed over SSH.
    All eight deterministic gates ran sequentially on that commit: seven
    passed with exit 0, and `check-ast-grep` took the Makefile's optional-skip
    branch because the host binary is not executable. The test gate ran 10,523
    tests with 10,523 passes and 23 skips. Evidence bundle recorded below.

## Surprises & discoveries

- Observation: Lody's version-1 task schema accepts only `pending`,
  `in_progress`, `completed` and `failed`; it does not accept `killed`.
  Evidence: `packages/shared/src/acp/claude-subagent-task.ts` in the
  checked-out Lody source defines the closed status enum used by
  `_meta.lody.task`. Impact: a closed child maps to `failed`, with its error
  retained, rather than the unsupported `killed` value in the draft risk
  mitigation.
- Observation: the EP-M1 red run failed at compile time because
  `lody_task_session_update` was intentionally absent. Evidence:
  `/tmp/red-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm1.out` reports
  `E0425` from the new task lifecycle unit test. Impact: the red test directly
  proves that the new standard ACP adapter is required; the existing private
  notification cannot satisfy it.
- Observation: EP-M1 passed the complete repository release gate and was
  committed as `16ba7b5160a28aab2fcf1d47cb8d37bc9c28892f`. Evidence:
  `/tmp/check-VTCode-fix-acp-lody-task-lifecycle-negotiation-2.out` records
  rustfmt, policy checks, Clippy, build, 6,582 tests, harness regressions and
  rustdoc passing. Impact: the standard lifecycle repair is a complete,
  independently releasable plateau.
- Observation: persisted background records are loaded into every controller
  for a workspace without an explicit owning ACP session, while background
  cancellation itself performs no ACP ownership check. Evidence:
  `SubagentController::new` loads `background_children` from workspace state;
  `BackgroundSubprocessEntry` contains generated runtime and exec session IDs
  but no parent ACP session ID. Impact: EP-M2 cannot prove `SESSION-ISOLATION`
  for background list, cancel or output using the existing controller state.
  This activates the plan's security tolerance and requires a deliberate
  persisted-model/API expansion or removal of background management from this
  stack.
- Observation: the current public background snapshot API hard-limits previews
  to 24 lines, while the approved Lody output contract specifies a caller
  default of 200 and a hard maximum of 10,000. Evidence:
  `SUBAGENT_PREVIEW_LINES` and `background_snapshot()` clamp both live and
  archived output before the ACP adapter can apply a requested tail. Impact:
  meeting the output contract also requires extending the controller API rather
  than adding only an ACP adapter.
- Observation: ACP SDK `ExtRequest` is a schema value, not a directly
  registerable typed request handler. Evidence:
  `ClientRequest::ExtMethodRequest` owns extension dispatch and the SDK strips
  the leading underscore before constructing `ExtRequest`. Impact: EP-M2 must
  use a custom untyped dispatcher or a carefully scoped `ClientRequest`
  catch-all that returns `Handled::No` for standard requests.
- Observation: filtering persisted records only when a controller loads is not
  sufficient isolation: an owner-scoped save would otherwise delete foreign and
  legacy records from the shared workspace state file. Evidence:
  `save_background_state` previously replaced the whole persisted record set
  with the controller's in-memory subset. Impact: scoped controllers now
  receive their real ACP session ID before restore and merge only their owner's
  records under a process-wide persistence lock; unscoped CLI controllers
  retain replace-all semantics.

- Observation: truthful background-task negotiation is controlled by
  `background_subagents_enabled`, not by the broader
  `managed_background_runtime` predicate. Evidence: the EP-M2 capability and
  duplex tests exercise the configured background-subagent feature flag
  directly. Impact: ACP advertises background tasks only when that capability
  is enabled; unrelated runtime state cannot cause an unsupported promise.

- Observation: EP-M2's red tests exposed absent ownership and management
  plumbing; the expanded core and ACP suites then passed the contract. Evidence:
  `/tmp/red-vtcode-core-epm2.out`, `/tmp/green-vtcode-core-epm2-final.out`
  (5/5),
  `/tmp/red-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm2-acp.out`, and
  `/tmp/test-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm2-final.out`
  (6/6). Impact: EP-M2 is complete; provider usage and custom-provider
  streaming opt-in are the remaining implementation work.

- Observation: VTCode already emits the model's task tracker as a standard ACP
  `Plan`, including blocked-task labels and durable replay. Evidence:
  `crates/codegen/vtcode-acp/src/zed/agent/task_progress.rs` and its duplex
  tests in `zed/agent/handlers.rs`. Impact: no new plan/task-tracker protocol
  is required. The Lody `tasks` capability will advertise only enabled
  background-subagent work.
- Observation: `_vtcode/taskLifecycle` is neither a standard ACP task update nor
  a Lody-recognized legacy extension, and its discriminator is incompatible
  with Lody's Claude compatibility parser. Evidence:
  `crates/codegen/vtcode-acp/src/zed/agent/task_lifecycle.rs` uses
  `message.type`; Lody commit `dd241fd4108ba5de2f7e2d8d627713152d812a06`
  recognizes only `_claude/taskLifecycle` and `_kimi/taskLifecycle` legacy
  carriers using a different shape. Impact: the custom notification must be
  removed rather than renamed.
- Observation: the existing `SubagentController` already exposes status,
  close/cancel, child snapshots and background output previews. Evidence:
  `vtcode-core/src/subagents/mod.rs`, `controller_spawn_run.rs` and
  `controller_background_ops.rs`. Impact: Lody management is an ACP adapter
  concern, not a new core subsystem.
- Observation: Baseten documents per-request token usage, cached input tokens
  and final streamed usage, but the final usage chunk is conditional on
  `stream_options.include_usage: true`. Evidence: Baseten's Chat Completions
  reference and GLM-5.3 Flash model page; VTCode currently adds the option only
  for native OpenAI in `vtcode-llm/src/providers/openai/provider/streaming.rs`.
  Impact: negotiated Lody usage would remain empty for the user's Baseten
  sessions without a provider-profile opt-in.
- Observation: Lody's canonical usage extension is the version-1
  `_lody/session/usage_update` method, and VTCode must advertise
  `usage: {version: 1}` even when a provider does not return usage. Evidence:
  the Lody Core usage method and the EP-M3 capability and mapping tests;
  `/tmp/test-vtcode-acp-fix-acp-lody-task-lifecycle-negotiation-epm3-final.out`.
  Impact: usage negotiation is stable across providers, while a response with
  no normalized usage produces no notification rather than invented counts.
- Observation: usage is emitted once per provider response, including each
  response in a tool loop, and the normalized terminal `choices: []` usage
  chunk is retained by the custom OpenAI-compatible adapter. Evidence: the
  official ACP duplex test and the custom-provider wiremock and vidai-mock
  tests recorded in the EP-M3 artefacts below. Impact: Lody receives additive
  deltas for intermediate and final responses, including Baseten-style terminal
  usage, without double-counting.
- Observation: custom-provider `supports_stream_usage` is sparse and defaults
  to false; resolved profile precedence controls the request, while native
  OpenAI request behaviour is unchanged. Evidence: the configuration precedence
  and request-shape tests in
  `/tmp/test-vtcode-config-fix-acp-lody-task-lifecycle-negotiation-epm3.out` and
  `/tmp/test-vtcode-llm-wiremock-fix-acp-lody-task-lifecycle-negotiation-epm3.out`.
  Impact: existing proxy configurations remain safe, and only an explicitly
  opted-in compatible endpoint receives `stream_options.include_usage`.
- Observation: the first standalone ACP usage duplex check exposed timing and
  extension-method normalization assumptions; the final test uses the official
  ACP extension channel and passes after those boundaries were made explicit.
  Evidence: the final green transcript is
  `/tmp/test-vtcode-acp-usage-duplex-fix-acp-lody-task-lifecycle-negotiation-epm3-green.out`.
  Impact: the behavioural test validates the wire contract rather than a
  direct helper invocation.
- Observation: Baseten's aggregate `/v1/model_apis/usage` endpoint is bucketed
  across requests and API keys. Evidence: Baseten's model API pricing and
  limits documentation. Impact: it is unsuitable for per-turn ACP usage and is
  out of scope.
- Observation: nested subagents were invisible to both lifecycle forwarding and
  management. Evidence: a nested controller is stored in the parent's
  `ChildRecord::child_controller`, not in the root controller's `children` map,
  and the ACP observer registry is keyed by the session passed to
  `register_progress_observer`. Impact: the root's status list, its observer
  and the management RPCs all missed grandchildren, and a nested controller
  published under its own child session key. The repair adds descendant
  traversal plus observer-scope inheritance rather than duplicating registries.
- Observation: cancelling an already-settled task rewrote its outcome. Evidence:
  `close_single` unconditionally set `Closed`, and background cancellation
  transitioned any non-`Running` record through the cancellation path. Impact:
  a client that re-sent cancel for a completed task observed a
  `failed`/`killed` task, so close and cancel now return the recorded terminal
  entry unchanged.
- Observation: a lifecycle forwarder attached after background restoration saw
  no events for tasks that were already running and never changed again.
  Evidence: the forwarder only receives transitions queued after observer
  registration, while restoration loads active records first. Impact: the
  forwarder now publishes an initial owned-and-active snapshot on attach.

## Decision log

- Decision: stack this work directly on PR #13 rather than the unpublished
  skill-discovery or context-limit branches in the original worktree.
  Rationale: the user requested a new stacked PR, and PR #13 is the latest
  published stack layer. This avoids silently coupling the integration to
  unrelated unpublished commits. Date/Author: 2026-08-28 / Codex.
- Decision: use standard ACP `ToolCall`/`ToolCallUpdate` messages with stable
  `task:<task-id>` identifiers and `_meta.lody.task` snapshots. Rationale: this
  is the Lody contract and remains meaningful to non-Lody ACP clients, unlike a
  provider-specific lifecycle notification. Date/Author: 2026-08-28 / Codex.
- Decision: negotiate `usage: {version: 1}` unconditionally,
  `tasks: {version: 1, background: true}` only when
  `background_subagents_enabled` is true, and `subagents` only when a
  controller exists. Advertise list, cancel and output only after their
  handlers land in the same coherent commit. Rationale: capabilities are
  promises, not product marketing. Date/Author: 2026-08-28 / Codex.
- Decision: emit usage as per-response deltas, not cumulative session totals.
  Rationale: Lody's usage tracking service sums incoming updates per ACP
  session; cumulative snapshots would double-count. Date/Author: 2026-08-28 /
  Codex.
- Decision: add `supports_stream_usage` to custom-provider profiles rather than
  special-casing Baseten hostnames. Rationale: the capability is part of the
  OpenAI-compatible peer contract, while hostname inference is brittle and
  would expose other proxies to known 400 responses. Date/Author: 2026-08-28 /
  Codex.
- Decision: extend persisted background records with an optional owning ACP
  session identifier, treat a missing identifier as legacy unowned state, and
  exclude such records from every ACP list, cancel and output operation. Add a
  controller API that returns a caller-requested output tail under a hard
  bound, while preserving the existing 24-line preview API for current callers.
  Rationale: explicit ownership is the only available basis for proving session
  isolation across process restarts. An optional additive field keeps old
  records deserializable without granting them new authority, and a new bounded
  API avoids silently changing existing preview behaviour. Date/Author:
  2026-08-28 / Codex, approved by the user as option 1.
- Decision: gate the advertised background-task capability with
  `background_subagents_enabled`. Rationale: negotiation must describe the ACP
  feature that is actually enabled, rather than infer support from a separate
  runtime-management predicate. Date/Author: 2026-08-28 / Codex.
- Decision: advertise Lody usage version 1 unconditionally, but emit the
  canonical `_lody/session/usage_update` only when a provider response carries
  normalized usage. Emit one delta per response, including every tool-loop
  response, and mirror the delta in the response model's `modelUsage` entry.
  Rationale: capability negotiation is stable, absent provider data is not
  fabricated, and Lody's accumulator expects additive per-response values.
  Date/Author: 2026-08-28 / Codex.
- Decision: make custom-provider streamed usage a sparse profile capability,
  defaulting to false and following resolved profile precedence; leave native
  OpenAI behaviour unchanged. Rationale: OpenAI-compatible proxies vary in
  support for `stream_options.include_usage`, so an explicit opt-in preserves
  existing request compatibility while enabling Baseten-shaped terminal usage.
  Date/Author: 2026-08-28 / Codex.
- Decision: carry the generated configuration reference as a full regeneration
  — 928 added and 926 removed lines — instead of the two-row change first
  intended. Rationale: `scripts/generate_config_field_reference.py` proved both
  `supports_stream_usage` schema paths exist; a later commit then regenerated
  the file and a formatting pass re-laid it out. `8de48ee03` restored the full
  JSON defaults the checked-in reference had truncated
  (`agent.onboarding.recommended_actions`, `commands.extra_path_entries`,
  `permissions.auto.block_rules` and peers), and the formatting pass re-laid
  the widened table at mdtablefix's canonical column widths. Keeping only the
  two new rows would have reverted those deliberate corrections and left the
  reference disagreeing with the schema, so the file is carried in full and its
  cost is recorded under Tolerances rather than hidden. Date/Author: 2026-08-28
  / Codex; revised 2026-10-31 / Claude Code.
- Decision: derive nested-task visibility from the existing controller tree
  instead of introducing a session-to-controller registry. Rationale: the tree
  is already the authority for which controller spawned which child, it is
  reachable only through records the session already owns, and a second
  registry would have to be kept in sync with spawn, restore and shutdown.
  Date/Author: 2026-10-31 / Claude Code.
- Decision: a terminal task is immutable to cancel. Rationale: Lody's task
  lifecycle is a monotonic ladder, so re-sending cancel for a completed or
  failed task must return the recorded outcome rather than manufacture a new
  terminal state. Date/Author: 2026-10-31 / Claude Code.
- Decision: publish an initial snapshot when the forwarder attaches rather than
  buffering restoration transitions. Rationale: the forwarder is attached per
  session and may attach after restoration has already completed; replaying the
  live owned-and-active snapshot is bounded, idempotent and does not resurrect
  terminal history the client never saw. Date/Author: 2026-10-31 / Claude Code.

## Outcomes & retrospective

EP-M1 landed as an independently gated lifecycle repair. Standard ACP clients
now receive task-shaped tool calls, Lody receives `_meta.lody.task`, and
capability negotiation is truthful. EP-M2 crossed its explicit security
tolerance only after the user approved an additive persisted-model expansion:
explicit owner identifiers, fail-closed handling of legacy ownerless records,
and bounded caller-selected output tails. The exact dispatcher now serves
negotiated list, output and cancel requests, with real duplex coverage for
owned and foreign tasks. The core and ACP focused suites passed (5/5 and 6/6
respectively). EP-M3 then added canonical per-response Lody usage updates,
profile-controlled custom-provider streamed usage and local Baseten-shaped
playback/physics coverage. EP-M4 documented the negotiated extensions and
custom-provider opt-in, passed the complete release gate, installed the user
binary and published draft PR #14 as the fourth layer of stack #16. The plan is
complete. Review feedback then exposed three correctness gaps that the original
milestones missed: nested controllers were not walked by lifecycle forwarding
or management, cancelling a settled task rewrote its terminal outcome, and a
forwarder attached after restoration never published already-active work. EP-M5
repaired all three through the existing controller tree and added the boundary
coverage those gaps implied. The scope deviation recorded under Tolerances is
the honest cost of the approved ownership expansion plus that repair round.
Provider limits, compaction metadata, notices, final-answer phase metadata and
scheduled tasks remain explicitly independent follow-ups.

## Context and orientation

VTCode's ACP server lives in `crates/codegen/vtcode-acp`. The SACP handler
registration and initialization response are in `src/zed/agent/handlers.rs`;
standard session notifications are sent through `src/zed/agent/updates.rs` and
`src/zed/connection.rs`. Per-session runtime state is in `src/zed/types.rs`.

`src/zed/agent/task_lifecycle.rs` subscribes to
`vtcode_core::subagents::SubagentProgressEvent`, filters events by parent ACP
session and currently emits the ignored `_vtcode/taskLifecycle` notification.
The controller that owns those events is in
`crates/codegen/vtcode-core/src/subagents`. It already provides status lists,
child shutdown, child transcript snapshots, background shutdown and background
output previews.

Lody extensions are negotiated in `agentCapabilities._meta.lody`. Version 1
defines `usage`, `tasks` and `subagents` independently. Session usage travels
through `_lody/session/usage_update`. Management uses `_lody/subagents/list`,
`_lody/subagents/cancel` and `_lody/subagents/output`. Lifecycle itself does
not use a custom method: it is a standard ACP tool call whose `_meta.lody.task`
value is a complete task snapshot.

The word "task" has three distinct meanings here. VTCode's model-facing
`task_tracker` is an ACP `Plan`. Lody's `tasks` capability says whether the
agent supports managed background or scheduled work. Lody task metadata is the
UI/history representation for subagents, background work or scheduled work.
Only the latter two are changed by this plan.

Provider responses carry `vtcode_commons::llm::Usage` with prompt, completion,
cache-read and cache-creation counts. OpenAI-compatible streams expose usage in
a terminal SSE chunk only when requested. The custom-provider OpenAI router
already parses a received usage chunk, but it does not request one from Baseten
today.

## Conformance basis

- User requirement, 2026-08-28: implement negotiated Lody usage, tasks and
  subagents, beginning with the standard task-lifecycle repair.
- ACP SDK `agent-client-protocol = 2.0.0`, schema 1.5.0, and ACP v1
  extensibility/tool-call contracts pinned by this repository.
- Lody extension contract `LodyAI/acp-extension-core` main revision
  `23c792b910a903b74601e346473827106f991715` (`capabilities.ts`, `methods.ts`,
  `session.ts`, `usage.ts`).
- Lody client/parser revision
  `dd241fd4108ba5de2f7e2d8d627713152d812a06` in `/home/leynos/Projects/Lody`.
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
  and no unimplemented scheduled support is advertised. Method: parameterized
  unit tests plus official ACP duplex behavioural tests. Rationale: the state
  space is the finite cross-product of controller absent, controller present
  and background disabled/enabled. Domain: all capability combinations VTCode
  can construct. Artefact: tests beside `advertised_agent_capabilities` and in
  the ACP duplex harness in `zed/agent/handlers.rs`. Evidence: the red test
  observes missing `_meta.lody`; the green test parses exact versioned
  capabilities and successfully invokes every advertised method. Non-vacuity:
  include witnesses for no controller, child-only controller and
  child-plus-background controller; a negative control advertises no
  `scheduled` key and rejects an unknown `_lody` method.
- Obligation: `TASK-IDENTITY`. Every lifecycle update for one VTCode task uses
  one stable `task:<id>` tool-call identifier, carries a complete valid
  `_meta.lody.task` snapshot and maps terminal state monotonically. Method:
  table tests for the finite status enums and a property test over generated
  valid lifecycle sequences. Rationale: examples prove wire shape while
  generated sequences catch accidental identifier changes, terminal regressions
  and incomplete metadata. Domain: child and background statuses,
  summaries/errors and timestamp presence across sequences of length 1 through
  32. Artefact: `task_lifecycle.rs` unit/property tests and official ACP duplex
  lifecycle test. Evidence: the existing custom-notification assertion fails
  after the red expectation is changed; green observes an initial `ToolCall`,
  subsequent `ToolCallUpdate` values and no `_vtcode/taskLifecycle`
  notification. Non-vacuity: classify child/background and
  pending/running/terminal cases; include a seeded ID-changing mapper that the
  stable-ID property rejects.
- Obligation: `SESSION-ISOLATION`. List, cancel and output never reveal or
  mutate a task outside the requested ACP session, including nested children.
  Method: proptest-generated task forests plus behavioural requests over the
  duplex ACP connection. Rationale: ancestry and mixed-session arrangements are
  combinatorial and deserve wider coverage than hand-picked examples. Domain:
  acyclic forests up to 32 tasks across two to four session roots, with
  foreground and background nodes and every status. Artefact: pure
  ownership-selection property tests and handler-level ACP tests. Evidence:
  owned nested tasks are listed and manageable; foreign IDs return the same
  not-found response as unknown IDs and remain unchanged. Non-vacuity: require
  generators to produce direct, nested, foreign and background tasks; a
  negative control that filters only direct children must fail on a generated
  nested witness.
- Obligation: `USAGE-DELTA`. Each provider usage sample produces exactly one
  non-negative Lody delta with the same input, output, cache-read and
  cache-creation counts, keyed by the response model; absence produces no
  notification. Method: property tests for numeric conversion and ACP
  behavioural tests. Rationale: generated boundaries exercise zero, ordinary
  and maximum `u32` values without relying on live billing. Domain: every field
  across `0..=u32::MAX`, provider usage present/absent and model identifiers
  including Unicode. Artefact: Lody usage-mapping tests and a duplex
  notification test. Evidence: red observes no usage notification; green parses
  a canonical `_lody/session/usage_update` whose per-model and aggregate deltas
  agree. Non-vacuity: classify zero/non-zero/cache/no-cache samples; a swapped
  prompt/completion negative control must fail.
- Obligation: `BASETEN-STREAM-USAGE`. A custom OpenAI-chat provider requests
  usage only when its resolved profile opts in, and a Baseten-shaped terminal
  usage chunk reaches the ACP Lody notification unchanged. Method:
  request-shape unit tests, wiremock-style deterministic provider playback and
  a vidai-mock physics test with streamed SSE chunks. Rationale: unit tests
  prove configuration, playback proves parser contract, and vidai-mock proves
  the assembled streaming path without external cost or nondeterministic
  network timing. Domain: provider default/profile override precedence,
  opt-in/out, content chunks followed by terminal usage, and usage absent
  despite opt-in. Artefact: custom-provider config/provider tests and the
  existing ACP provider fixture environment extended with a Baseten usage
  scenario. Evidence: request capture contains
  `stream_options.include_usage: true` only for the opted-in profile; Lody
  receives one delta after the terminal chunk. Non-vacuity: the opt-out request
  is asserted to omit `stream_options`; a fixture with no terminal usage must
  produce no notification.

External axioms are limited to ACP's documented extension routing and Lody Core
revision `23c792b9` accepting the stated version-1 shapes. Baseten's documented
final usage chunk is treated as a peer contract; the repository-owned request
construction and parsing are still exercised against faithful local boundaries.
No formal prover or model checker is proportionate: the introduced invariants
are finite serialization, ownership filtering and monotone event mapping
without concurrency shared-memory semantics beyond the existing broadcast
channel.

EP-M5 evidence addendum (2026-10-31):

- `TASK-IDENTITY`: `subagents` projection property test
  `lifecycle_sequences_keep_stable_ids_and_monotonic_terminality` generates
  child and background lifecycle walks of length 1 to 32, asserts one stable
  `task:child-1` identifier, asserts only the first projection is a `ToolCall`,
  and asserts the projected terminality ordinal never regresses. The seeded
  ID-changing style negative control is the table test
  `maps_worker_and_background_states_to_lody_task_metadata`, which pins the
  identifier for every enum value.
- `SESSION-ISOLATION`: the ownership closure is proven combinatorially and
  behaviourally.
  `lody_tests.rs::ownership_selection_is_the_transitive_session_closure_of_a_mixed_forest`
  generates acyclic mixed forests of 3 to 32 tasks across two to four session
  roots, with foreground-controller entries of every status, covers the entry's
  own `background` flag, forces a background witness in every case, delivers
  the entries in shuffled order, and compares the answer with the
  generation-order answer. Its negative control is a direct-child-only filter
  that a generated nested witness rejects.
  `lody_tests.rs::background_ownership_is_the_owner_session_field` covers the
  separate managed-background path used by list, cancel and output:
  `lody::owned_background_ids` selects by `owner_session_id`, and the property
  generates records owned by the requested session, by a foreign root and by no
  session at all, in shuffled order, with an owner-agnostic filter as its
  negative control. Behaviourally,
  `subagents/tests/tree_routing_tests.rs::descendant_controllers_include_only_attached_child_scopes`
  and `descendant_controllers_deduplicate_shared_child_scopes` prove the walk
  reaches exactly the attached child scopes, once each.
  `nested_controller_publishes_under_the_root_observer_scope` and
  `nested_scope_is_inherited_through_more_than_one_level` prove a grandchild
  and great-grandchild publish under the root session key while the child key
  receives nothing. The official ACP duplex test
  `nested_subagent_management_is_owned_by_the_root_session` lists a child and
  two grandchildren, cancels one owned grandchild directly through the
  descendant controller, confirms the sibling and the parent stay running, and
  asserts that a foreign task returns exactly the same error value as an
  unknown task for both output and cancel. It also asserts that a background
  task which was already running is published when the forwarder attaches,
  closing the observation gap that allowed a running task to stay invisible.
- Attach-time publication accounting:
  `task_lifecycle/projection_tests.rs::initial_snapshot_enqueue_shares_accounting_with_live_updates`
  mixes an awaited initial snapshot with a live `try_send`, checks depth
  before and after each dequeue, and covers the closed-channel and full-channel
  paths, so neither enqueue source can cancel the other's tracked depth.
  `task_lifecycle/projection_tests.rs::the_forwarder_never_observes_an_unaccounted_item`
  exercises the concurrency boundary over 256 rounds on a multi-threaded
  runtime: publication and counting share one critical section, so the receiver
  sees depth one before it dequeues and an empty channel leaves depth, the
  metric count and the drain count at zero.
- Settled-outcome preservation is covered by
  `close_preserves_settled_foreground_outcomes` and
  `cancel_preserves_settled_background_outcomes`, which table the terminal
  statuses and assert status, completion/end timestamps and error text survive
  a cancel.
- `USAGE-DELTA` assembled coverage is
  `crates/codegen/vtcode-acp/src/zed/agent/handlers/tests/tools/baseten_usage.rs`,
  which starts a real custom-provider profile against a wiremock
  OpenAI-compatible origin, drives the ACP prompt pipeline, and asserts the
  terminal-usage chunk produces exactly one `_lody/session/usage_update` while
  a usage-free stream publishes no delta; the same test also asserts the opt-in
  `stream_options.include_usage` request shape.
  `crates/codegen/vtcode-acp/src/zed/agent/tests/streamed_audit.rs` remains the
  synthetic-provider projection check, and
  `crates/codegen/vtcode-llm/tests/vidaimock_streaming.rs` remains the parser
  boundary check.

## Plan of work

EP-M1 adds a private `zed/agent/lody.rs` module containing the versioned
subagent-lifecycle capability and task-metadata DTOs, then changes
`task_lifecycle.rs` to retain its broadcast subscription, session filter and
lifetime while replacing the custom extension notification. The forwarder will
remember which stable task IDs it has emitted: the first snapshot becomes
`SessionUpdate::ToolCall`, and later snapshots become
`SessionUpdate::ToolCallUpdate`. Both carry `_meta.lody.task` and a normal ACP
status/title. The obsolete method and legacy-shaped message builders will be
deleted. `handle_initialize` will advertise only
`subagents: {version: 1, lifecycle: true}` when an actual controller exists.

EP-M2 extends the private Lody module with typed management DTOs and registers
one `ExtRequest` dispatcher in `handlers::install_handlers`. Exact
`_lody/subagents/list`, `/cancel` and `/output` methods will parse typed
parameters, resolve the requested `SessionHandle` and controller, establish
task ownership, then call existing controller operations. List will combine
child and managed-background entries in deterministic ID order. Cancel will use
child `close` or background `force_cancel_background`. Output will render a
bounded tail (default 200 lines, hard maximum 10,000) from the existing child
snapshot or background preview. Unknown or foreign task IDs will share one
not-found response. The same commit will add `list`, `cancel` and `output`
negotiation flags, plus `tasks: {version: 1, background: true}` only when the
`background_subagents_enabled` capability is enabled.

EP-M3 emits a canonical `_lody/session/usage_update` after each provider
response that contains normalized usage, including every intermediate tool-loop
response. `usage` and the matching `modelUsage[model]` entry are per-response
deltas; a response without usage emits no notification. The custom-provider
profile gains sparse `supports_stream_usage` opt-in with resolved profile
precedence, while native OpenAI request behaviour remains unchanged. The
existing stream decoder preserves terminal `choices: []` usage. Nested Baseten
cached/reasoning detail remains optional: cached tokens are included only when
represented reliably in normalized `Usage`, and reasoning tokens are not
fabricated.

EP-M4 updates `docs/acp/ACP_INTEGRATION.md`, `docs/acp/ACP_QUICK_REFERENCE.md`,
the custom-provider configuration reference and any generated schema fixture
required by the established config workflow. It then runs release gates,
installs the user binary, updates this plan with evidence, pushes the branch
and opens a draft PR based on `fix/acp-unresolved-tool-recovery`.

Each milestone is test-first and ends with focused tests, full commit gates, a
commit and a user-level binary installation before the next milestone begins.

## Milestones and plateaus

- Identifier and outcome: EP-M1, standard task lifecycle visible to Lody and
  generic ACP clients with truthful lifecycle-only negotiation. Requirements
  and gaps: discharges USER-LIFECYCLE and advances USER-SUBAGENTS. Acceptance
  evidence: lifecycle duplex and property tests pass; custom method is absent;
  initialize advertises lifecycle only when a controller exists. Conformance
  check: existing event/filter/lifetime contracts remain intact, and no
  management handler is advertised prematurely. Recovery: revert the lifecycle
  commit; no persisted migration exists. Remaining gaps: management and usage.
  Compatibility decision: no compatibility layer for `_vtcode/taskLifecycle`;
  it had no documented consumer and Lody ignored it.
- Identifier and outcome: EP-M2, session-scoped Lody subagent/background-task
  management and truthful management/task negotiation. Requirements and gaps:
  discharges USER-SUBAGENTS and USER-TASKS. Acceptance evidence:
  list/cancel/output duplex tests and ownership property tests pass.
  Conformance check: no cross-session data, management flags match handlers,
  and scheduled support remains omitted. Recovery: revert handler registration
  and remove its advertised flags. Remaining gaps: provider usage.
  Compatibility decision: none; these are new versioned extension methods.
- Identifier and outcome: EP-M3, provider usage reaches Lody, including
  explicitly opted-in Baseten streams. Requirements and gaps: discharges
  USER-USAGE. Acceptance evidence: configuration tests (4/4), custom-provider
  wiremock playback (1/1), ACP usage and tool-loop tests (9/9), the official
  ACP usage duplex test (1/1), and vidai-mock physics (1/1) pass; see the EP-M3
  artefact block below. Conformance check: usage is canonical and additive,
  absent metrics stay absent, and the proxy-safe default remains off. Recovery:
  disable the profile flag or revert the usage commit. Remaining gaps:
  documentation/publication only. Compatibility decision: the new optional
  config field defaults off, so existing custom-provider request bodies remain
  byte-for-byte compatible.
- Identifier and outcome: EP-M4, documented, gated, installed and published
  stacked PR. Requirements and gaps: closes documentation and delivery
  requirements. Acceptance evidence: release gates, binary version/path, remote
  SHA, PR base and draft state. Conformance check: reconcile every discovery
  and trace link before marking COMPLETE. Recovery: use `gh stack push` after
  corrections; never force-push manually. Remaining gaps: provider limits,
  compaction metadata, notices, final-answer phase metadata and scheduled tasks
  remain independent follow-ups. Compatibility decision: none.

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

Before publication, inspect and submit the complete stack non-interactively:

```bash
gh stack link 11 12 13 14
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

EP-M2 Robust Core Expansion and Management evidence:

```plaintext
RED:   /tmp/red-vtcode-core-epm2.out
GREEN: /tmp/green-vtcode-core-epm2-final.out
       5/5 core tests passed, including owner persistence, legacy loading and
       bounded output-tail properties.
RED:   /tmp/red-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm2-acp.out
GREEN: /tmp/test-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm2-final.out
       6/6 ACP tests passed, including real duplex negotiation, owned
       list/output/cancel and foreign-task indistinguishability from unknown.
GREEN: /tmp/rgr-core-owner-scope-green.out
       Owner-scoped loading includes only the exact owner while the CLI remains
       unscoped.
GREEN: /tmp/rgr-core-owner-persistence-green.out
       Scoped persistence updates owned state without deleting foreign or
       legacy records.
GREEN: /tmp/rgr-acp-owner-scope-green.out
       A new ACP session does not restore another session's background record.
```

EP-M3 Provider Usage evidence:

```plaintext
GREEN: /tmp/test-vtcode-config-fix-acp-lody-task-lifecycle-negotiation-epm3.out
       4/4 configuration parsing and profile-precedence tests passed.
GREEN: /tmp/test-vtcode-llm-wiremock-fix-acp-lody-task-lifecycle-negotiation-epm3.out
       1/1 custom-provider request-shape and terminal-usage playback test passed.
GREEN: /tmp/test-vtcode-acp-fix-acp-lody-task-lifecycle-negotiation-epm3-final.out
       9/9 ACP capability, usage-mapping, tool-loop and recovery tests passed.
GREEN: /tmp/test-vtcode-acp-usage-duplex-fix-acp-lody-task-lifecycle-negotiation-epm3-green.out
       1/1 official ACP extension-channel usage duplex test passed.
GREEN: /tmp/test-vtcode-llm-vidaimock-fix-acp-lody-task-lifecycle-negotiation-epm3.out
       1/1 Baseten-shaped terminal usage physics test passed.
FORMAT: /tmp/fmt-check-VTCode-fix-acp-lody-task-lifecycle-negotiation-epm3.out
        Captured the EP-M3 formatting check and its rustfmt diagnostics; the
        final release gate remains the authoritative clean-format check.
RED:   /tmp/test-vtcode-acp-usage-duplex-fix-acp-lody-task-lifecycle-negotiation-epm3.out
       The initial duplex assertion exposed ACP's normalized extension method
       name (the leading underscore is stripped) and notification timing.
       The official-channel green rerun above covers the corrected boundary.
GATE:  /tmp/check-VTCode-fix-acp-lody-task-lifecycle-negotiation-epm3-2.out
       Full release gate passed: 6588 tests passed and 16 were skipped, with
       formatting, security, logging, governance, Clippy, build, PTY/TUI and
       documentation checks green.
INSTALL: /tmp/install-vtcode-fix-acp-lody-task-lifecycle-negotiation-epm3.out
         Commit b18a674dc installed successfully at user level; the existing
         locked-yanked chacha20 0.10.1 warning was non-fatal.
SCHEMA: /tmp/generate-config-reference-VTCode-fix-acp-lody-task-lifecycle-negotiation-epm4.out
        Exported 836 schema fields successfully; only the two feature-specific
        reference rows are retained because the remaining generated diff was
        unrelated pre-existing drift.
```

EP-M5 second review repair release-gate evidence (2026-11-01, candidate =
`7519a004` plus the review-repair working tree; gates run sequentially, once
each, by the scrutineer):

```plaintext
FORMAT:  /tmp/check-fmt-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 8s. rustfmt clean; mdtablefix left 365 files unchanged.
LINT:    /tmp/lint-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 227s. Shell syntax, repository policy scripts, workspace
         Clippy with --all-targets --all-features -D warnings, and cargo doc.
TYPE:    /tmp/typecheck-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 158s. cargo check --locked --workspace --all-targets
         --all-features.
TEST:    /tmp/test-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 317s. 10,521 tests run, 10,521 passed, 23 skipped.
BUILD:   /tmp/build-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 221s.
HARNESS: /tmp/test-harness-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 589s. 8 PTY, 10 pipe and 49 inline-event harness tests passed.
AST-GREP: /tmp/check-ast-grep-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0 through the Makefile's optional-skip branch. The only installed
         ast-grep binary (/home/leynos/.local/bin/ast-grep, mode 0600) is not
         executable, so the repository scan did not run on this host. Recorded
         as an environment limitation, not as passing scan evidence.
ADVISORY: /tmp/advisory-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 2s. File-length, unwrap/expect, zen-allowlist and
         agent-legibility advisories completed in warn mode; the unwrap/expect
         scan reported 0 findings across 1,990 files.
INSTALL: /tmp/install-r7-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         cargo install --path . --locked --force replaced the user binary with
         vtcode-core 0.156.1 in 21m50s; /home/leynos/.local/bin/vtcode
         resolves to /home/leynos/.cargo/bin/vtcode and reports 0.156.1.
```

EP-M5 third review repair release-gate evidence (2026-11-01, candidate =
`60b2c4cc5`; gates run sequentially, once each, by the scrutineer):

```plaintext
FORMAT:  /tmp/check-fmt-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, under 1s. rustfmt clean; mdtablefix left 365 files unchanged.
LINT:    /tmp/lint-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0. Workspace Clippy with --all-targets --all-features
         -D warnings and cargo doc finished clean.
TYPE:    /tmp/typecheck-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 303s. cargo check --workspace --all-targets --all-features;
         sccache was not serving, so the check compiled uncached.
TEST:    /tmp/test-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 103s. 10,523 tests run, 10,523 passed, 23 skipped.
BUILD:   /tmp/build-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0, 195s. cargo build --locked --workspace.
HARNESS: /tmp/test-harness-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0. 8 PTY, 10 pipe and 49 inline-event harness tests passed.
AST-GREP: /tmp/check-ast-grep-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0 through the Makefile's optional-skip branch. The installed
         ast-grep binary remains mode 0600, so no scan ran; recorded as an
         environment limitation, not as passing scan evidence.
ADVISORY: /tmp/advisory-r8-vtcode-fix-acp-lody-task-lifecycle-negotiation.out
         EXIT 0. File-length, unwrap/expect, zen-allowlist and
         agent-legibility advisories completed in warn mode with 0
         unwrap/expect findings and 0 allowlist or legibility gaps.
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
Core revision `23c792b9` exactly. These types are an adapter boundary, not a
new public crate API.

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

______________________________________________________________________

Revision note (2026-08-28): EP-M4 is complete. Canonical per-response usage
updates, custom-provider streamed-usage opt-in, wiremock playback, vidai-mock
physics and official ACP duplex coverage are recorded above. The complete
release gate and user installation passed, and draft PR #14 is the fourth layer
of stack #16.
