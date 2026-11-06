# Make ACP interrupted-tool recovery durable and atomic

This ExecPlan is a living document. It records the review-repair plateau for
pull request #13 only. Its required living sections are `Constraints`,
`Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`, `Decision log`,
`Outcomes & retrospective`, `Conformance basis`, and `Verification plan`.

Status: IN PROGRESS (2026-11-05)

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
  transcripts. Severity: high. Likelihood: medium. Mitigation: introduce or use
  the required-checkpoint path only at pre-tool admission.
- Risk: archive repair may confuse duplicate terminal evidence for a canonical
  provider result. Severity: high. Likelihood: medium. Mitigation: retain the
  archive byte-for-byte and fail closed before registration or continuation
  whenever a result cannot be attributed one-for-one to its assistant batch.

## Progress

- [x] (2026-09-08) Verified exact remote head and created this clean worktree
  from `aa434f916`.
- [x] (2026-09-08) Vetted review IDs `3791762676`, `3791762677`,
  `3944816962`, `3944816966`, and `3944816968` against the exact source.
- [x] (2026-09-08) Stopped an unauthorized artisan focused-test command before
  it executed tests; retained its partial compilation log as non-evidence.
- [x] (2026-09-08) Added source-only red candidates for four review findings;
  no test result is recorded because the shared scrutineer is the sole runner.
- [x] (2026-09-08) Root approved a narrowly additive synchronous atomic
  mutation method and conservative late-result relocation policy.
- [x] (2026-09-08) Added durable-admission and archive-registration source
  coverage, plus the fail-closed checkpoint helper and delayed registration.
  Static diff inspection is clean; no test result is recorded.
- [x] (2026-09-08) Added atomic session-lock mutation with explicit
  synchronization coverage, and conservative duplicate/non-contiguous recovery
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
- [x] (2026-09-08) Froze the approved six-path pre-merge manifest and added
  module/API documentation plus bounded recovery observations. Traces carry
  static outcome labels and existing identifiers/counts only; they do not
  record tool arguments, results, archive contents, or a new metric/event
  stream. The source change awaits the sole shared scrutineer's scoped packet.
- [x] (2026-11-02) Obtained the remaining ACP/core relevant tests, Clippy and
  documentation checks through the shared scrutineer. `make check-fmt`,
  `make lint`, `make typecheck`, and `make test` all passed on the committed
  candidate; the focused ACP recovery tests passed as part of that run.
- [x] (2026-11-02) Obtained root-scheduled deterministic gates, CodeRabbit
  review, and the required per-thread dispositions after a verified commit. The
  reviewed head was green in CI, every inline finding was repaired or rebutted
  with evidence, and each thread was answered and resolved.
- [x] (2026-11-02) Reconciled the five remaining pre-merge warnings on head
  `6ab1a3f73`. Docstring coverage: added docstrings to every diff-touched
  function the 75.00% measurement flagged. User-facing documentation: added the
  stale `write_stdin` section to `docs/users-guide.md`. Developer
  documentation: added the history-mutation, archive-registration, and
  stale-session contracts to `docs/developers-guide.md` and marked this plan
  complete. Domain architecture: replaced the JSON-reading
  `is_stale_exec_session` policy check with a typed
  `ToolExecutionError::stale_exec_session` field and rendered `details` only at
  serialization time. Observability: added the `acp_tool_execution` span, a
  bounded error category, `InterruptedCall` for live interruptions, and
  `StaleWriteStdinRequest` for post-input session loss. The candidate awaits
  the shared scrutineer's four-gate run.
- [x] (2026-11-02) The four-gate run found two `vtcode-core` failures in the
  typed stale-session work. The registry returns tool failures as a JSON
  envelope, so the retry loop re-parsed the error and lost the typed
  classification (`stale_exec_session` is `#[serde(skip)]`), after which the
  generic mutating-tool heuristic marked a pre-write stale continuation as
  partial state. `from_error_payload` now rehydrates the typed classification
  from the rendered `details` before any policy decision, and a round-trip
  regression test covers it. The other failure was a test still reading the
  client-facing `details` field instead of `rendered_details()`.
- [x] (2026-11-02) Re-ran the four gates sequentially on the repaired tree and
  published it as `228a5d9f8`. `make check-fmt` passed (364 Markdown files
  unchanged), `make lint` passed (shell, policy, structured-logging, Clippy with
  `-D warnings`, and `cargo doc`), `make typecheck` passed, and `make test`
  passed 10,524 tests with 22 skipped and no failures. The remote head was
  verified with `ls-remote` and the tracking ref refreshed.
- [x] (2026-11-02) Closed the review's remaining observability and
  documentation packet.
  `successful_tool_round_emits_acp_tool_execution_span_and_replaced_counter`
  asserts the `acp_tool_execution` span, the `results_replaced` outcome,
  `elapsed_ms`, and the `ReplacedCall` counter;
  `interrupted_tool_round_emits_span_and_fixed_error_category` asserts the
  `execution_interrupted` outcome, `elapsed_ms`, a fixed `error_category` with
  no raw error field, and the `InterruptedCall` counter. The stale-session
  tests now assert `StaleWriteStdinRequest` for both the pre-input modes and
  the post-input session loss. The ExecPlan, ACP runtime guide, metric
  documentation, README, and documentation index were reconciled with the
  completed state. The candidate awaits the shared scrutineer's four-gate run.
- [x] (2026-11-02) The shared scrutineer ran the four gates sequentially on the
  reconciled tree and all passed: `make check-fmt` (exit 0, 364 Markdown files
  unchanged), `make lint` (exit 0), `make typecheck` (exit 0), and `make test`
  (exit 0; 10,526 passed, 0 failed, 22 skipped). Published as `141ce90d6` over
  SSH; `ls-remote` confirmed the remote head and the tracking ref was
  refreshed. Posted the per-check disposition reply at
  <https://github.com/leynos/vtcode/pull/13#issuecomment-6584778594> through
  the comment-token pool. The `CodeRabbit` check for the new head reads
  "Skipped - Review paused"; the existing queue request `25da8dc2` remains
  pending and is reused rather than duplicated.
- [x] (2026-11-03) Reconciled the second pre-merge walkthrough (comment
  `5307347085`, updated on head `1baf4aea5`). Module-level documentation: added
  the `//!` contract to `exec_sessions.rs`. Unit architecture: made
  `resolve_exec_session` a pure read and moved `StaleWriteStdinRequest`
  recording into the three `execute_command_session_*` command paths through
  the named `record_stale_write_stdin_lookup` helper. Domain architecture:
  serialised the typed `stale_exec_session` classification as its own envelope
  field, removed the `details` rehydration path, and made the Prometheus family
  transport-neutral. User-facing documentation: added the ACP recovery, archive
  repair, and stale `write_stdin` entries to `docs/releases/0.157.0.md`.
  Docstring coverage: added doc comments to the 68 diff-touched functions the
  71.70% measurement flagged.
- [x] (2026-11-03) Rebased the branch onto the advanced parent
  `4e8642a5d` (`feat/model-visible-file-versions-noop-patch-guard`), which the
  PR had reported as `CONFLICTING`. The only textual conflict was the
  `metrics/mod.rs` re-export block: the parent added
  `PatchCertificationOutcome` plus `record_runtime_revalidation` and
  `record_patch_certification`, while this branch added
  `tool_recovery_metrics`. Both sides were retained. Weave auto-resolved the
  remaining sibling changes and reported no residual conflict markers. The
  rebased candidate is `df8d04104`; its four-gate run is pending on the shared
  scrutineer.
- [x] (2026-11-03) The parent advanced again to `dabe6b57e` (four further
  commits: RustSec remediation, ACP read-certification extraction, patch
  handler and registry test-module splits, and regenerated third-party
  notices), so the PR was `CONFLICTING` once more. A read-only
  `git merge-tree --write-tree` predicted a clean Weave auto-merge, and the
  rebase confirmed it: no textual conflict and no residual markers. The rebased
  candidate is `fac8e0b37`; the four repository gates are being run
  sequentially on it.
- [x] (2026-11-03) The four gates passed on `66bd7f21d`, the amended candidate
  after `make fmt` rewrapped the preceding plan entry: `make check-fmt` (364
  files unchanged), `make lint` (exit 0, Clippy clean, documentation generated),
  `make typecheck` (exit 0, 56m43s), and `make test` (10,541 passed, 0 failed,
  22 skipped, exit 0). Published the candidate over SSH and verified the remote
  head before refreshing the tracking ref. Posted the pre-merge reconciliation
  comment at <https://github.com/leynos/vtcode/pull/13#issuecomment-6608152013>
  through the comment-token pool. CodeRabbit accepted four of the five findings
  in reply <https://github.com/leynos/vtcode/pull/13#issuecomment-6608191238>:
  module-level documentation, unit architecture, user-facing documentation, and
  domain architecture are resolved in source, while docstring coverage stays
  unverified until a completed analysis reports its denominator and percentage
  against the 80% threshold. Queue request `9dfbd795` remains pending for the
  review that supplies it; no second request was added.
- [x] (2026-11-03) The parent advanced once more to `0ef2b65de`
  (`refactor: deduplicate read-certification and patch-safety setup`). A
  read-only `git merge-tree --write-tree` again predicted a clean merge, and
  the rebase reproduced that exact tree (`021afd1c6`), so the rebased candidate
  contains the parent's refactor without textual conflict or residual markers.
  The candidate is now `16e345811`; the four repository gates are being run
  sequentially on it.
- [x] (2026-11-03) The four gates passed on `dfe463a06`, which adds this plan
  record to the rebased candidate: `make check-fmt` (364 files unchanged),
  `make lint` (exit 0, Clippy clean, documentation generated), `make typecheck`
  (exit 0, 40m30s), and `make test` (10,547 passed, 0 failed, 22 skipped, exit
  0). Published over SSH with a force-with-lease pinned to the superseded head
  `66bd7f21d`; `ls-remote` confirmed the new remote head and the tracking ref
  was refreshed. The push regenerated `refs/pull/13/merge`, but the new merge
  ref (`3b04355f`) still merges into the same stack base `2396140df`, so the
  duplicate-baseline result was not superseded; see the corrected diagnosis
  under Surprises.
- [x] (2026-11-03) Pushed the plan record as `b83434d1d` after the four gates
  passed on `dfe463a06`, then investigated the inherited
  `Check Unmaintained Dependencies` failure on the new merge ref
  `3b04355f`. The failure reproduces identically on PR #12's own merge base
  (`2396140df`), so it is parent-owned. The PR #13 branch cannot alter the
  other parent's contribution to that merge. Watched run `41692498795`
  (head `b83434d1d`): formatting, Clippy, Markdown, RustSec, MSRV, license,
  custom invariants, unwrap/expect, workflow security, and Zen governance
  passed; the unmaintained-dependency job was still running; Windows and
  Cargo nextest were still running; CodeRabbit remained skipped at
  `18:18:18Z` ahead of the queued review `9dfbd795`.
- [x] (2026-11-05) Verified and repaired the four inline findings from the
  completed review at `b83434d1d`. `exec_sessions.rs` now records
  `StaleWriteStdinRequest` only when the invoked tool is `write_stdin`, with the
  tool identity threaded through the wait path; `tool_recovery.rs` extracts
  `tool_result_group_end` and `completed_tool_call_ids` and recomputes the group
  after relocation; `handlers/tests/session.rs` is split into
  `session/concurrent_prompt.rs` (281 lines remain in the parent); and the
  ExecPlan's registration-order claim is corrected.
- [x] (2026-11-05) Closed the two pre-merge rows. Added the
  `execute_tool_round` duplicate-ID integration test in
  `handlers/tests/prompt.rs` (asserts rejection, unchanged history, unchanged
  loop counter, and zero staged/replaced/interrupted counters) and three
  focused `insta` snapshots: the recovery placeholder JSON, the stale-session
  wire envelope, and the fixed `vtcode_tool_recovery_total` Prometheus series.
- [x] (2026-11-05) Raised the `unmaintained` job timeout from 20 to 30 minutes
  with an explanatory comment; scans have taken 14-16 minutes and one job was
  cancelled at 20m16s. This is an availability fix, not a suppression: the
  scan still fails on a real baseline violation.

## Surprises & discoveries

- The pre-existing dirty repair worktree is based on a different ancestry:
  `git merge-base aa434f916 HEAD` there is `1aa90f19`, not `aa434f916`. It is
  therefore unsuitable as a publication source, even though it contains related
  exploratory changes.
- A Red-test artisan incorrectly ran a focused nextest command despite the
  exclusive-scrutineer rule. Its owned process was interrupted; the log ends in
  compilation interruption and proves no test result. Future artisan packets in
  this programme may author source and return a selector, but may not run tests
  or any gate.
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
  deletions, or +572 non-plan lines. It remains within the root-approved +600
  tolerance. The only public interface addition is the approved existing handle
  method; no wire or archive format changes were made.
- CodeScene reports the four observations in
  `ambiguous_archive_is_not_resumed_or_replayed_and_preserves_the_archive` as a
  large assertion block. They are one failure contract for one resume attempt:
  an actionable error, no registered session, byte-identical archive, and
  unchanged external target. Splitting or weakening them would obscure the
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
- The approved bounded artisan and scribe assignments could not be dispatched:
  both attempts returned `agent thread limit reached`. No active agent was
  interrupted. Root authorized direct journeyman implementation of the frozen
  six-file packet, while preserving the independent shared-scrutineer review
  and gate requirement.
- A typed field that is `#[serde(skip)]` does not survive the registry's own
  JSON error envelope. `execute_public_tool_ref_dispatch` converts a failed
  tool call into `Ok(error_payload)`, and the retry loop parses it back with
  `ToolExecutionError::from_tool_output`. Any policy state that exists only in
  the typed field is therefore lost on that path, so the wire payload must be
  rehydrated before `with_tool_call_context` runs.
- The `Check Unmaintained Dependencies` failure on `66bd7f21d`
  (`duplicate baseline finding identity at baseline.entries[33]`) is a
  persistent property of the stacked synthetic base, not a stale merge ref that
  a new head clears. After publishing `b83434d1d`, GitHub rebuilt
  `refs/pull/13/merge` as `3b04355f`, whose parents are still
  `2396140df` (the stack base) and the new branch head. `2396140df` merges
  `0ef2b65de` (parent branch) into `ce140d638` (main). The parent branch's
  baseline has 32 entries with one `terminfo 0.9.0` at `Age 365`; main's has 34
  entries with `memmem 0.1.1`, `quick-error 2.0.1`, and `terminfo 0.9.0` at
  `Age 368`. The textual merge keeps both `terminfo` entries, producing 35
  entries. The checker normalises `{ "Age": N }` to the identity `"Age"`, so
  the two entries collide as the same reviewed finding and
  `_index_baseline_entries` raises the duplicate error at index 33.
- The failure is therefore inherited from the parent branch's mergeability
  preview, not introduced by this branch. PR #12 fails its own
  `Check Unmaintained Dependencies` identically on the same merge base:
  `error: duplicate baseline finding identity at baseline.entries[33]`, job
  `125973058233`, run `41652215888`. No edit to this branch's 32-entry baseline
  can remove an entry contributed by the other parent of the merge; deleting
  this branch's `terminfo` entry would be wrong-scoped and merge-fragile. The
  fix belongs to the parent PR: its branch must merge or otherwise reconcile
  with main `ce140d638` and keep a single reviewed `terminfo 0.9.0` entry
  (main's `Age 368`), after which the synthetic merge for this PR carries one
  entry. Owner: the PR #12 lifecycle; next action: update the parent branch
  from main and reconcile the baseline, then rerun both PRs' checks.
- The unmaintained-dependency job has a second, independent reliability problem:
  `.github/workflows/ci.yml:219` sets `timeout-minutes: 20`, while the scan step
  routinely takes 14-16 minutes. Job `125973058233` (PR #12) took 11m33s;
  job in run `41664625459` took 15m23s; the successful job in run
  `41669725353` took 14m12s. On head `b83434d1d`, job `126114928567` started at
  `18:19:15Z` and was cancelled by the workflow timeout at `18:39:31Z` while
  still scanning, so the whole CI run is reported `cancelled` even though every
  other job passed (Cargo nextest at `18:34:07Z`, Windows at `18:28:08Z`).
  Raising the timeout to 30 minutes is a bounded availability fix, not a
  suppression: it does not change the scan, its exit code, or the baseline
  policy. It should be applied after the parent-owned duplicate is reconciled
  so the check can return a determinate result.

## Decision log

- Decision: reconcile CodeRabbit's suggestion to log a failed repair checkpoint
  with the Codex durability finding by checkpointing before session
  registration and returning an error on failure. Rationale: this preserves the
  documented uncertain-effect recovery contract; an in-memory repaired session
  with a stale archive is not safe. Date/Author: 2026-09-08 / ACP stack
  convergence journeyman, approved by root.
- Decision: keep the atomic mutation primitive in the existing thread runtime
  rather than adding a second ACP lock. Rationale: one authoritative lock
  prevents lost updates across ACP and other thread appenders. Date/Author:
  2026-09-08 / ACP stack convergence journeyman.
- Decision: treat all artisan test execution as out of bounds, including a Red
  test. Red/Green commands are exclusively scheduled through the shared
  scrutineer unless a separate alchemist experiment is explicitly authorized.
  Rationale: shared Cargo state and the project gate policy require one serial
  verifier; a source-only artisan cannot create validation evidence.
  Date/Author: 2026-09-08 / ACP stack convergence journeyman, directed by root.
- Decision: add `ThreadRuntimeHandle::mutate_messages<T>` as a narrow atomic
  closure operation over the already-authoritative session mutex. Its closure
  must be short, synchronous and non-reentrant; it must not call the handle,
  perform I/O, or await while the mutex is held. Rationale: it retains the
  single state owner and existing API compatibility, prevents lost updates, and
  crosses the crate boundary without a second lock. Date/Author: 2026-09-08 /
  root-approved ACP stack convergence decision.
- Decision: canonicalize a uniquely attributable late Tool result by moving it
  beside its assistant call while preserving exact terminal payload and the
  relative order of unrelated messages. Do not replay, discard, or replace a
  known terminal effect. Treat duplicate, ambiguous, or conflicting identities
  as conservative uncertainty and escalate if the representation cannot retain
  all evidence. Rationale: provider-valid contiguity cannot justify loss of
  durable effect evidence. Date/Author: 2026-09-08 / root-approved ACP stack
  convergence decision.
- Decision: carry the restored archive's existing `turn_diagnostics` into the
  one repair checkpoint, while ordinary subsequent checkpoints retain their
  existing absence of explicit diagnostics. Rationale: repair must not silently
  erase a known durable outcome merely to satisfy the expanded
  progress-checkpoint contract. Date/Author: 2026-09-08 / root-directed
  focused-gate repair.
- Decision: reject an archive before repair persistence, session registration,
  provider generation, or tool execution when duplicate/conflicting results
  cannot be attributed one-for-one to one assistant tool-call batch. Preserve
  its on-disk bytes and return an actionable ACP error. Reused IDs in distinct,
  completed batches remain permitted. Rationale: durable evidence cannot be
  deleted or guessed into a wire-valid result, and provider serializers differ
  in their unsafe duplicate handling. Date/Author: 2026-09-08 / root-approved
  ACP stack convergence decision.
- Decision: retain the four assertions in the ambiguous-archive admission test
  as one atomic failure contract instead of reducing CodeScene's large-block
  count by splitting them. Rationale: they observe the same rejected resume
  attempt across its caller, in-memory registration, durable archive, and
  external side-effect boundary; separate attempts would no longer prove the
  required fail-closed outcome. Date/Author: 2026-09-08 / root-directed PR13
  CodeScene triage.
- Decision: treat the `handlers.rs` file-size finding as a tracked baseline
  concern (#118), while reducing only the newly added checkpoint test's method
  size through a private fixture. Rationale: a whole-module reduction exceeds
  this recovery plateau, but the new admission coverage must remain explicit
  and reviewable. Date/Author: 2026-09-08 / root-directed PR13 CodeScene triage.
- Decision: raise the non-plan-line ceiling from +600 to +650 only for the
  current CodeScene follow-up and any formatter wrapping it requires.
  Rationale: the coherent fixture and single-batch validator produce +603
  lines, and reducing that count by minifying or weakening assertions would
  damage the required evidence without reducing feature scope. Date/Author:
  2026-09-08 / root-approved PR13 CodeScene tolerance decision.
- Decision: satisfy recovery observability with the existing tracing surface:
  stable outcome labels with session/thread identifiers and call/result counts.
  Do not create an ACP metric collector, event bus, payload logging, or log
  while a thread-history mutex is held. Rationale: this makes durable admission
  and recovery outcomes diagnosable without widening the runtime contract or
  exposing tool inputs/results. Date/Author: 2026-09-08 / root-approved PR13
  pre-merge packet.
- Decision: when agent capacity prevented the approved disjoint artisan/scribe
  packets, complete the mechanical six-file change directly as journeyman;
  retain independent verification through the root-designated scrutineer.
  Rationale: no agent was available without disrupting concurrent ACP work.
  Date/Author: 2026-09-08 / root authorized.

## Outcomes & retrospective

Complete as of 2026-11-02. The plateau delivered a narrow, reviewable #13
repair with no changes outside recovery/history integrity, apart from the
approved `ThreadRuntimeHandle` API addition. The historical Red stage was not
executed against this clean worktree because the shared scrutineer was the sole
runner; an interrupted artisan attempt is not evidence. That limitation is
retained here rather than rewritten.

The pre-merge documentation and tracing amendment is implemented and validated.
It provides explicit fail-closed archive-resume guidance, documented strict
admission sequencing, and bounded static recovery observations. The shared
scrutineer supplied formatter, test, lint, and documentation evidence on the
committed candidate, and the reviewed head was green in CI.

Retrospective: the exclusive-scrutineer rule was the right control, but it
needs an explicit packet for every gate so a long compile queue is not mistaken
for a hang. The typed stale-session classification also showed that
client-facing `details` must never double as policy state; the follow-up
separates the two.

## Context and orientation

`crates/codegen/vtcode-acp/src/zed/agent/handlers.rs` stages model tool calls,
persists a write-ahead session checkpoint, then calls `execute_tool_calls`.
`session_state.rs` reconstructs archive sessions, repairs their tool-call
history, checkpoints that repaired history with `CheckpointDurability::Required`,
and only then registers the resumed session handle. `tool_recovery.rs` stages,
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
PR13-RECOVERY -> PR13-PREMERGE -> docs and bounded recovery observations
```

## Verification plan

`INV-ADMISSION-DURABLE`: before `execute_tool_calls`, a required checkpoint
either succeeds or the handler returns an ACP error and the side-effect fake is
not called. Use an async unit/behavioural test with a failing archive target;
the negative control is the old best-effort helper, which would execute the
fake and fail the call-count assertion.

`INV-REPAIR-REGISTER`: a failed repaired-archive checkpoint leaves
`session_handle(session_id)` absent; a retry after a valid destination
succeeds. Use the existing session-state fixture. The directory-as-file target
is a reachable failure witness and the retry proves the test did not merely
reject all loads.

`INV-HISTORY-ATOMIC`: one thread-history mutation observes and writes under one
lock, so an append cannot be discarded between a snapshot and replacement. Use
the approved `mutate_messages<T>` primitive and explicit synchronization to
hold the mutation while an append attempts to enter. The test must prove the
append is retained after release; it must use no timing or sleep criterion.

`INV-TOOL-CLOSURE`: a uniquely attributable result separated by a non-Tool
message is relocated beside its assistant call with its exact payload
preserved. Duplicate, ambiguous, or conflicting evidence remains represented
without guessing or deletion; therefore the strict one-result canonical form is
asserted only for unambiguous fixtures. Use table-driven cases plus existing
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

`INV-RECOVERY-OBSERVABILITY`: recovery observations record only a static
outcome label plus existing session/thread identifiers and counts. Placeholder
staging, terminal replacement, interrupted execution, rejected ambiguous
archives, concurrent prompts, and stale exec sessions retain their existing
control flow and wire errors. Compile/lint coverage verifies the additions;
existing behavioural selectors verify the unchanged recovery path without
requiring a log-capture dependency.

The only external axiom is the filesystem's reported checkpoint error. Tests
exercise repository-owned error handling using an invalid archive destination;
they do not assume third-party filesystem internals.

## Plan of work

Stage A confirms the exact methods and callers using CodeGraph/LSP navigation
and reads their crate-local instructions. Stage B adds the four failing tests
without production changes and records the intended red result. Stage C adds a
fallible required-checkpoint path at the tool admission boundary, defers
session-map insertion until repair persistence succeeds, introduces the minimal
atomic thread-history mutation operation, and normalizes archive recovery.
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
messages, and fail-closed ambiguous archive continuation. The conformance check
confirms no wire/archive schema change and only the approved minimal public
`ThreadRuntimeHandle::mutate_messages<T>` addition. At the time `EP-M2` closed,
the remaining work was the ACP/core relevant gates, Clippy, documentation
checks and external review; the Progress section records that work as completed.

`EP-M3` ends after root-scheduled gates and review convergence. Its recovery is
to retain the focused commit and re-run the gate packet; publication is outside
this plateau. The only compatibility decision is the approved additive method
on the existing public thread handle; all other changed interfaces are internal.

## Concrete steps

Run all commands in
`/home/leynos/Projects/VTCode.worktrees/acp-pr13-review-convergence`.

1. Add focused tests and return their selectors to the root-designated
   scrutineer. The scrutineer, not an artisan, runs
   `cargo nextest run --locked -p vtcode-acp <test filters>` with
   `RUSTFLAGS='-D warnings'`. The initial source-only repair began under the
   exclusive-runner constraint, so the historical red run is absent; the
   scrutineer records the focused green result against the completed repair and
   the plan records this limitation.
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
is not acceptance. At the time this plan was written, completion required the
remaining ACP/core relevant behavioural evidence, Clippy, documentation checks,
a committed exact head, and review-thread replies with `@coderabbitai` after
the commit exists. The Progress section records each of those as obtained.

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
other ACP layers. Revised after an artisan started an unauthorized focused
test: the plan now makes the shared scrutineer the sole executor of Red/Green
commands as well as repository gates. Revised again to record the
public-interface and archive-canonicalization escalations, then root's approved
narrow atomic mutation and evidence-preserving relocation decisions. The final
approved tolerance is 600 net non-plan lines in the existing four owned files,
including the ambiguity guard and its load/continuation controls.

2026-09-08: reconciled the live plan after focused gates. It now records the
approved additive public `mutate_messages<T>` method, +572 non-plan line delta,
and exact focused green logs. The planned Red stage remains a design oracle,
not claimed execution evidence; the remaining packet is ACP/core relevant
tests, Clippy and documentation checks before commit and review convergence.

2026-09-08: added the root-approved six-path documentation and observability
follow-up. Agent capacity prevented the planned bounded artisan/scribe split,
so root authorized direct journeyman implementation without interrupting other
ACP work. The independent shared scrutineer remains the only validation
authority. Published review dispositions remain deferred until a verified,
rebased SHA exists and use the `pandalump` token process when authorized.
