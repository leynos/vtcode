# Make ACP interrupted-tool recovery durable and atomic

This ExecPlan is a living document. It records the review-repair plateau for
pull request #13 only. Its required living sections are `Constraints`,
`Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`, `Decision log`,
`Outcomes & retrospective`, `Conformance basis`, and `Verification plan`.

Status: IN PROGRESS

## Purpose / big picture

An ACP session must never execute a tool after its write-ahead record could not
be made durable, and a repaired archive must never become an addressable active
session before its repair is durable. Concurrent history updates must not erase
messages. After this plateau, a failed checkpoint produces a visible ACP error
before a tool side effect, a failed archive repair leaves no live session, and
an archive with ambiguous terminal results is rejected before continuation.

## Constraints

- Work from commit `aa434f916882918047a24fa39d3d95256fa6a1c6` in this clean
  worktree. Preserve the dirty `acp-pr13-review-repair` worktree as evidence;
  do not copy or publish its ancestry.
- Limit production work to ACP checkpointing/recovery and the existing core
  thread-history handle. Do not alter public ACP wire formats, archived session
  formats, tool replay policy, permissions, or provider behaviour.
- Keep checkpoint failure fatal at the pre-tool write-ahead boundary while
  durable history is enabled. Ordinary transcript checkpoints retain their
  existing best-effort behaviour.
- Add no dependency, compatibility wrapper, new event contract, or broad
  refactor. `ThreadRuntimeHandle` may gain one minimal closure-based atomic
  mutation method only if its existing locking contract permits it.
- Only the root-designated scrutineer runs repository gates and CodeRabbit.
  Do not push, rebase, restack, comment, request review, mark PR state, or
  merge in this plateau.

## Tolerances (exception triggers)

- Stop and escalate if the repair needs more than eight tracked production or
  test files, more than 650 net non-plan lines, a public API change other than
  the root-approved `ThreadRuntimeHandle::mutate_messages<T>` method, a new
  dependency, or a persistent/wire-format migration. The +650 ceiling applies
  only to the current CodeScene follow-up, including formatter wrapping.
- Stop if atomic mutation cannot be expressed inside the existing
  `ThreadRuntimeHandle` locking model, or if the correct checkpoint policy is
  ambiguous after examining its callers.
- Stop after two focused-test repair attempts or any deterministic failure that
  is unrelated to this diff. Preserve the failure log for the scrutineer.

## Risks

- Risk: an atomic read-modify-write helper can deadlock if callers hold the
  thread lock. Severity: high. Likelihood: low. Mitigation: inspect callers and
  test only closure-local mutation without re-entering the handle.
- Risk: treating every checkpoint as fatal could regress ordinary session
  transcripts. Severity: high. Likelihood: medium. Mitigation: introduce or
  use the required-checkpoint path only at pre-tool admission.
- Risk: archive repair may confuse duplicate terminal evidence for a canonical
  provider result. Severity: high. Likelihood: medium. Mitigation: retain the
  archive byte-for-byte and fail closed before registration or continuation
  whenever a result cannot be attributed one-for-one to its assistant batch.

## Progress

- [x] (2026-09-08) Verified exact remote head and created this clean worktree
  from `aa434f916`.
- [x] (2026-09-08) Vetted review IDs `3791762676`, `3791762677`,
  `3944816962`, `3944816966`, and `3944816968` against the exact source.
- [x] (2026-09-08) Stopped an unauthorised artisan focused-test command before
  it executed tests; retained its partial compilation log as non-evidence.
- [x] (2026-09-08) Added source-only red candidates for four review findings;
  no test result is recorded because the shared scrutineer is the sole runner.
- [x] (2026-09-08) Root approved a narrowly additive synchronous atomic
  mutation method and conservative late-result relocation policy.
- [x] (2026-09-08) Added durable-admission and archive-registration source
  coverage, plus the fail-closed checkpoint helper and delayed registration.
  Static diff inspection is clean; no test result is recorded.
- [x] (2026-09-08) Added atomic session-lock mutation with explicit
  synchronisation coverage, and conservative duplicate/non-contiguous recovery
  coverage. Unique late results are relocated intact; duplicate evidence is
  retained. Static diff inspection is clean; no test result is recorded.
- [x] (2026-09-08) Traced recovered ACP history into `LLMRequest` and provider
  serializers. OpenAI transiently skips a second result, while OpenResponses
  serializes both; no shared cardinality validator exists. Added a load-time
  ambiguity guard that leaves the archive untouched and prevents registration,
  provider continuation, and tool replay. Separate completed assistant batches
  may reuse an ID when each result remains unambiguous.
- [x] (2026-09-08) Repaired the ACP focused-gate compile blocker by importing
  `SnapshotTurnDiagnostics` through its existing public snapshots path, then
  manually applied the five supplied formatter hunks. Source-only diff checking
  is clean; the shared scrutineer must still run formatter and focused tests.
- [x] (2026-09-08) The shared scrutineer passed the deterministic core lock
  interleaving test in `/tmp/acp-pr13-terminal-admission-core-1.out`, then
  passed the six ACP recovery/admission tests and formatter in
  `/tmp/acp-pr13-terminal-admission-{acp,fmt}-2.out`.
- [x] (2026-09-08) Triaged `/tmp/acp-pr13-repair-codescene-1.json`. The
  write-ahead checkpoint test now keeps its ACP request and causal assertions
  in the test while a private fixture owns only archive/provider setup. The
  recovery loop now finds its pre-relocation boundary without an unused result
  collection and validates each completed assistant batch in a private helper.
  Source-only diff checking is clean; the shared scrutineer remains the sole
  validation runner.
- [x] (2026-09-08) Root approved a narrow +650 non-plan-line ceiling for this
  CodeScene follow-up, including formatter wrapping. The exact +603 delta is
  within that ceiling; no assertion was removed or code minified to meet the
  superseded +600 count.
- [ ] Obtain the remaining ACP/core relevant tests, Clippy and documentation
  checks through the shared scrutineer before a commit proposal.
- [ ] Obtain root-scheduled deterministic gates, CodeRabbit review, and the
  required per-thread dispositions after a verified commit.

## Surprises & discoveries

- The pre-existing dirty repair worktree is based on a different ancestry:
  `git merge-base aa434f916 HEAD` there is `1aa90f19`, not `aa434f916`.
  It is therefore unsuitable as a publication source, even though it contains
  related exploratory changes.
- A Red-test artisan incorrectly ran a focused nextest command despite the
  exclusive-scrutineer rule. Its owned process was interrupted; the log ends in
  compilation interruption and proves no test result. Future artisan packets
  in this programme may author source and return a selector, but may not run
  tests or any gate.
- The requested atomic repair needs a cross-crate API. Root approved a narrow
  method on the existing session mutex, constrained to short synchronous and
  non-reentrant closures with no handle calls, I/O, or await while locked.
- The first non-contiguous-result test candidate discarded a late real Tool
  result. Root instead approved relocation of a uniquely attributable result
  into its assistant's contiguous block, preserving its exact payload and all
  unrelated-message order. Duplicate, ambiguous, or conflicting result IDs are
  not guessed or silently discarded.
- An atomic-recovery artisan left a syntactically incomplete two-file draft.
  The journeyman stopped that hand-off, retained only the approved owned-file
  direction, and completed integrated source repair. No test, formatter, lint,
  review or publication command ran during that recovery.
- Root approved a one-time +400-net-line tolerance for these exact four files.
  The current +388 lines retain the actual ACP side-effect boundary and a
  deterministic lock-interleaving witness; it is a correctness-validation
  exception, not a general growth allowance. No further scope expansion is
  permitted without a new decision.
- The first focused gate compiled the ACP test target before running selectors
  and exposed two integration omissions: archive-progress checkpoints now
  require turn diagnostics, and `Result::expect_err` requires a Debug success
  type. The repair preserves durable diagnostics only for the restored archive
  repair checkpoint and uses an explicit match in the test; no public Debug
  implementation is added. No selector executed in that failed build.
- Duplicate durable results must remain in the archive as effect evidence, but
  are not a safe canonical provider continuation. This is a current ACP
  admission guard, not a new archive format or provider-history projection.
- The ambiguity guard and its two load/recovery controls raise the four-file
  source delta from +400 to +580 net lines. Root approved the +600 tolerance
  because both controls are required to distinguish a real duplicate result
  from legitimate ID reuse across completed assistant batches.
- The completed four-file production/test delta is 597 additions and 25
  deletions, or +572 non-plan lines. It remains within the root-approved
  +600 tolerance. The only public interface addition is the approved existing
  handle method; no wire or archive format changes were made.
- CodeScene reports the four observations in
  `ambiguous_archive_is_not_resumed_or_replayed_and_preserves_the_archive` as
  a large assertion block. They are one failure contract for one resume
  attempt: an actionable error, no registered session, byte-identical archive,
  and unchanged external target. Splitting or weakening them would obscure the
  causal relation and could permit an unsafe partial failure, so the metric is
  deliberately retained with this evidence.
- The `handlers.rs` file-size warning predates this narrow repair (#118). The
  added checkpoint test and its private fixture are accounted for as scoped
  admission evidence: the CodeScene-focused refactor is `+75/-52` lines from
  committed PR13 head `738381075`, a net +23 private-test-fixture lines. The
  warning is not waived and no whole-module reduction is attempted in this
  recovery plateau.
- The CodeScene-only source changes raise the four-file PR13 delta from +572
  to +603 non-plan lines. This exceeds the root-approved +600 limit by three
  lines. Root then approved a one-purpose +650 ceiling that also covers
  formatter wrapping; it does not authorize feature growth or scope expansion.

## Decision log

- Decision: reconcile CodeRabbit's suggestion to log a failed repair checkpoint
  with the Codex durability finding by checkpointing before session registration
  and returning an error on failure.
  Rationale: this preserves the documented uncertain-effect recovery contract;
  an in-memory repaired session with a stale archive is not safe.
  Date/Author: 2026-09-08 / ACP stack convergence journeyman, approved by root.
- Decision: keep the atomic mutation primitive in the existing thread runtime
  rather than adding a second ACP lock.
  Rationale: one authoritative lock prevents lost updates across ACP and other
  thread appenders.
  Date/Author: 2026-09-08 / ACP stack convergence journeyman.
- Decision: treat all artisan test execution as out of bounds, including a Red
  test. Red/Green commands are exclusively scheduled through the shared
  scrutineer unless a separate alchemist experiment is explicitly authorised.
  Rationale: shared Cargo state and the project gate policy require one
  serial verifier; a source-only artisan cannot create validation evidence.
  Date/Author: 2026-09-08 / ACP stack convergence journeyman, directed by root.
- Decision: add `ThreadRuntimeHandle::mutate_messages<T>` as a narrow atomic
  closure operation over the already-authoritative session mutex. Its closure
  must be short, synchronous and non-reentrant; it must not call the handle,
  perform I/O, or await while the mutex is held.
  Rationale: it retains the single state owner and existing API compatibility,
  prevents lost updates, and crosses the crate boundary without a second lock.
  Date/Author: 2026-09-08 / root-approved ACP stack convergence decision.
- Decision: canonicalise a uniquely attributable late Tool result by moving it
  beside its assistant call while preserving exact terminal payload and the
  relative order of unrelated messages. Do not replay, discard, or replace a
  known terminal effect. Treat duplicate, ambiguous, or conflicting identities
  as conservative uncertainty and escalate if the representation cannot retain
  all evidence.
  Rationale: provider-valid contiguity cannot justify loss of durable effect
  evidence.
  Date/Author: 2026-09-08 / root-approved ACP stack convergence decision.
- Decision: carry the restored archive's existing `turn_diagnostics` into the
  one repair checkpoint, while ordinary subsequent checkpoints retain their
  existing absence of explicit diagnostics.
  Rationale: repair must not silently erase a known durable outcome merely to
  satisfy the expanded progress-checkpoint contract.
  Date/Author: 2026-09-08 / root-directed focused-gate repair.
- Decision: reject an archive before repair persistence, session registration,
  provider generation, or tool execution when duplicate/conflicting results
  cannot be attributed one-for-one to one assistant tool-call batch. Preserve
  its on-disk bytes and return an actionable ACP error. Reused IDs in distinct,
  completed batches remain permitted.
  Rationale: durable evidence cannot be deleted or guessed into a wire-valid
  result, and provider serializers differ in their unsafe duplicate handling.
  Date/Author: 2026-09-08 / root-approved ACP stack convergence decision.
- Decision: retain the four assertions in the ambiguous-archive admission test
  as one atomic failure contract instead of reducing CodeScene's large-block
  count by splitting them.
  Rationale: they observe the same rejected resume attempt across its caller,
  in-memory registration, durable archive, and external side-effect boundary;
  separate attempts would no longer prove the required fail-closed outcome.
  Date/Author: 2026-09-08 / root-directed PR13 CodeScene triage.
- Decision: treat the `handlers.rs` file-size finding as a tracked baseline
  concern (#118), while reducing only the newly added checkpoint test's method
  size through a private fixture.
  Rationale: a whole-module reduction exceeds this recovery plateau, but the
  new admission coverage must remain explicit and reviewable.
  Date/Author: 2026-09-08 / root-directed PR13 CodeScene triage.
- Decision: raise the non-plan-line ceiling from +600 to +650 only for the
  current CodeScene follow-up and any formatter wrapping it requires.
  Rationale: the coherent fixture and single-batch validator produce +603
  lines, and reducing that count by minifying or weakening assertions would
  damage the required evidence without reducing feature scope.
  Date/Author: 2026-09-08 / root-approved PR13 CodeScene tolerance decision.

## Outcomes & retrospective

This section is incomplete. The current result is a narrow, reviewable #13
repair with focused green evidence and no changes outside recovery/history
integrity, apart from the approved `ThreadRuntimeHandle` API addition. The
historical Red stage was not executed against this clean worktree because the
shared scrutineer was the sole runner; an interrupted artisan attempt is not
evidence. The plan therefore records the honest source-to-green limitation
and will add the remaining gate, review, commit, and disposition evidence
before the plateau is complete.

## Context and orientation

`crates/codegen/vtcode-acp/src/zed/agent/handlers.rs` stages model tool calls,
persists a write-ahead session checkpoint, then calls `execute_tool_calls`.
`session_state.rs` reconstructs archive sessions and currently registers a
handle before persisting repaired tool-call history. `tool_recovery.rs` stages,
replaces and repairs tool-call result messages. `ThreadRuntimeHandle` in
`crates/codegen/vtcode-core/src/core/threads.rs` owns the canonical message
history lock. A *write-ahead checkpoint* is the durable placeholder recording
that a requested external effect may be uncertain before execution begins.

## Conformance basis

`ACP-GOAL-DURABILITY` is the user's standing ACP hardening objective: preserve
uncertain effects and do not promise replay safety. `PR13-RECOVERY` is pull
request #13's interrupted-tool recovery scope. Review records are Codex
`3791762676` and `3791762677`, and CodeRabbit `3944816962`, `3944816966`, and
`3944816968`, all against the exact head `aa434f916`.

```plaintext
ACP-GOAL-DURABILITY -> PR13-RECOVERY -> EP-M1 -> checkpoint failure test
ACP-GOAL-DURABILITY -> PR13-RECOVERY -> EP-M1 -> atomic history property test
PR13-RECOVERY -> EP-M2 -> repaired archive behavioural tests
```

## Verification plan

`INV-ADMISSION-DURABLE`: before `execute_tool_calls`, a required checkpoint
either succeeds or the handler returns an ACP error and the side-effect fake is
not called. Use an async unit/behavioural test with a failing archive target;
the negative control is the old best-effort helper, which would execute the
fake and fail the call-count assertion.

`INV-REPAIR-REGISTER`: a failed repaired-archive checkpoint leaves
`session_handle(session_id)` absent; a retry after a valid destination succeeds.
Use the existing session-state fixture. The directory-as-file target is a
reachable failure witness and the retry proves the test did not merely reject
all loads.

`INV-HISTORY-ATOMIC`: one thread-history mutation observes and writes under one
lock, so an append cannot be discarded between a snapshot and replacement. Use
the approved `mutate_messages<T>` primitive and explicit synchronisation to
hold the mutation while an append attempts to enter. The test must prove the
append is retained after release; it must use no timing or sleep criterion.

`INV-TOOL-CLOSURE`: a uniquely attributable result separated by a non-Tool
message is relocated beside its assistant call with its exact payload
preserved. Duplicate, ambiguous, or conflicting evidence remains represented
without guessing or deletion; therefore the strict one-result canonical form
is asserted only for unambiguous fixtures. Use table-driven cases plus existing
proptest support. Each generated fixture includes at least one assistant call;
classify duplicate, contiguous, and non-contiguous cases so the property cannot
pass vacuously.

`INV-AMBIGUOUS-RESUME`: an archive with duplicate/conflicting tool results is
rejected before registration, archive persistence, provider continuation, or
tool replay; its exact on-disk bytes and side-effect sentinel remain unchanged.
Repeated IDs in two completed assistant batches are a non-ambiguous control.

`INV-CODESCENE-RECOVERY`: the post-relocation recovery group computes its
boundary before relocation and collects completed IDs exactly once afterwards;
the private batch validator preserves duplicate, conflicting and orphan-result
rejection while accepting repeated IDs in distinct completed batches. The
checkpoint test retains its real ACP request, caller-visible checkpoint error,
one provider-call count and unchanged target assertion. Re-run its one handler
selector and the three recovery selectors through the shared scrutineer.

The only external axiom is the filesystem's reported checkpoint error. Tests
exercise repository-owned error handling using an invalid archive destination;
they do not assume third-party filesystem internals.

## Plan of work

Stage A confirms the exact methods and callers using CodeGraph/LSP navigation
and reads their crate-local instructions. Stage B adds the four failing tests
without production changes and records the intended red result. Stage C adds a
fallible required-checkpoint path at the tool admission boundary, defers
session-map insertion until repair persistence succeeds, introduces the minimal
atomic thread-history mutation operation, and normalises archive recovery.
Stage D runs focused tests, formatter and the root-scheduled gate packet. The
journeyman then prepares a semantic diff, commit proposal, and exact reply text
for each review thread; root coordinates publication and review requests.

## Milestones and plateaus

`EP-M1` specified red tests for all four defects. Its historical acceptance
evidence is unavailable on this clean worktree because exclusive shared-runner
scheduling began after the source repair. The source tests and focused green
results remain the current evidence; the interrupted artisan run is explicitly
non-evidence. Recovery is `git restore --staged` of only newly added tests.
There is no compatibility layer.

`EP-M2` ends after the narrow production repair and focused green tests. Its
acceptance evidence is no tool fake call on checkpoint error, no registered
failed repair, preserved concurrent history, canonical unambiguous repaired
messages, and fail-closed ambiguous archive continuation.
The conformance check confirms no wire/archive schema change and only the
approved minimal public `ThreadRuntimeHandle::mutate_messages<T>` addition.
Remaining work is the ACP/core relevant gates, Clippy, documentation checks
and external review.

`EP-M3` ends after root-scheduled gates and review convergence. Its recovery is
to retain the focused commit and re-run the gate packet; publication is outside
this plateau. The only compatibility decision is the approved additive method
on the existing public thread handle; all other changed interfaces are internal.

## Concrete steps

Run all commands in `/home/leynos/Projects/VTCode.worktrees/acp-pr13-review-convergence`.

1. Add focused tests and return their selectors to the root-designated
   scrutineer. The scrutineer, not an artisan, runs `cargo nextest run --locked
   -p vtcode-acp <test filters>` with `RUSTFLAGS='-D warnings'`. The initial
   source-only repair began under the exclusive-runner constraint, so the
   historical red run is absent; the scrutineer records the focused green
   result against the completed repair and the plan records this limitation.
2. Make the smallest edits in `handlers.rs`, `session_state.rs`,
   `tool_recovery.rs`, and, if necessary, `core/threads.rs`.
3. Run `cargo fmt --all -- --check`, the focused nextest packet, and the
   root-scheduled full packet serially. Expected final output is successful
   formatter and no focused test failures.
4. Use `sem diff --from aa434f916 --to HEAD` with bounded output before a
   commit proposal. The only expected semantic changes concern checkpoint
   admission, archive registration, history mutation, recovery and their tests.

## Validation and acceptance

The original red-stage expectations remain the design oracle, but no valid
clean-worktree Red result exists. The shared scrutineer has instead recorded
focused green behaviour for the completed repair. A successful formatter alone
is not acceptance. Completion requires the remaining ACP/core relevant
behavioural evidence, Clippy, documentation checks, a committed exact head,
and review-thread replies with `@coderabbitai` after the commit exists.

## Idempotence and recovery

The clean branch remains local until root approves publication. Re-running the
tests is safe. If an edit exceeds a tolerance, preserve it in the worktree,
record the failure in this plan, and escalate rather than copying from the
dirty historical worktree or force-pushing a stack branch.

## Interfaces and dependencies

`ThreadRuntimeHandle::mutate_messages<T>` is the root-approved additive public
closure operation over the canonical `Vec<Message>` while its existing lock is
held. Its closure must be short, synchronous and non-reentrant, with no handle
calls, I/O or await while locked. It does not create a second state owner. No
external dependency, wire interface, or archive-format interface is added.

## Revision note

2026-09-08: created from the exact PR #13 review inventory and root-approved
bounded repair packet. It deliberately excludes restacking, publication and all
other ACP layers. Revised after an artisan started an unauthorised focused test:
the plan now makes the shared scrutineer the sole executor of Red/Green commands
as well as repository gates. Revised again to record the public-interface and
archive-canonicalisation escalations, then root's approved narrow atomic
mutation and evidence-preserving relocation decisions. The final approved
tolerance is 600 net non-plan lines in the existing four owned files,
including the ambiguity guard and its load/continuation controls.

2026-09-08: reconciled the live plan after focused gates. It now records the
approved additive public `mutate_messages<T>` method, +572 non-plan line delta,
and exact focused green logs. The planned Red stage remains a design oracle,
not claimed execution evidence; the remaining packet is ACP/core relevant
tests, Clippy and documentation checks before commit and review convergence.
