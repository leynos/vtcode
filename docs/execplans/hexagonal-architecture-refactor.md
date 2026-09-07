# Deliver the hexagonal architecture remediation programme

Status: IN PROGRESS under the user's standing authorization to implement,
review and merge each issue on green without per-plan approval.

## Purpose / big picture

Deliver issue #39 as independently reviewable fixes and a staged application
extraction. The final observable result is one text–model–tool–model turn
scenario running through ACP and a headless driver with equivalent semantic
events, replaceable adapters, shared retry and recovery policy, and an enforced
inward dependency boundary. More interfaces or renamed directories alone do
not satisfy the programme.

## Conformance basis

The user supplied issue #39's full programme requirements. Live issues #21,
\#40–#47, #49 and #50 were read on 7 September 2026. No separate Terms of
Reference was supplied. Preserve existing architecture/event guidance and the
source-pinned evidence in those issues. The audit reviewed separate trains:
baseline `86ced9a6ce851c3e49f7aac5124d1d9285e8ba42` and capability
`d49d44ad5070e53c6c8e11a55bb520c6b67438b4`; neither is a combined integration.
Current main and the programme branch start at
`1aa90f196dacfb40ac96beb6e5aeafb7784a7d10`.

## Constraints

Preserve public wire formats, archives, aliases, permission/sandbox behaviour,
LLMProvider, ThreadRuntimeHandle, canonical ThreadEvent, prepared calls,
batching, and protocol/manifest-based extensions. Do not introduce another
event bus, provider abstraction or public Rust plugin framework. Keep arbitrary
MCP/plugin payloads extensible and schema validated. Tool/RPC success, process
exit, child completion and uncertain effects remain distinct. Never replay
uncertain mutating effects or promise exactly-once execution.

Do not duplicate #35/#36 or run paid live probes. Preserve the read, recovery,
lifecycle and usage behaviour from PRs #11–#17. Work in owned worktrees; do
not reset other agents' branches or kill their processes. Use the shared Cargo
cache, no build target under /tmp. Stop and notify the user if storage fills.
All gates are sequential and delegated exclusively to scrutineer.

## Tolerances (exception triggers)

Root owns integration choices, detailed issue plans, stack metadata, external
reviews and merges. Journeymen execute one approved issue/milestone and
delegate bounded measurable changes to artisans, reconnaissance to wyvern and
gates to scrutineer. Every worker has explicit file ownership. Workers
escalate scope changes to root before expanding. Root resolves routine choices
within the programme authorization; only contradictory/incomplete requirements
that prevent completion require user input. No elapsed-time limit applies.

Each issue plan sets local scope limits. No milestone may advance with failed
deterministic gates or unresolved applicable CLI review concerns. No PR may
merge with red CI or outstanding actionable review defects. Out-of-scope valid
findings must have an issue and a tagged explanation to CodeRabbit.

## Risks

Both pre-existing trains remain open and other worktrees exist on this host.
Inspect actual ancestry, working status and remote SHA before each mutation.
Capability PR #12 has inherited compile/CI failures; keeping #40 narrow
requires repairing prerequisites separately. The baseline train is actively
queued for review; do not duplicate its formatting and module work.

The comenq queue currently extends beyond thirteen hours. Queue only after
local gates, record the ETA, and use the wait for independent authorized work.
CLI review rate limits require `vsleep` for a random 45–90 minutes before
retry. CodeGraph indexing timed out after 300 seconds; verify file provenance
and use the known-path/Leta fallback; Context Pack MCP handles handoffs.

## Progress

- [x] (2026-09-07) Load requested skills, root guidance and live issue
  requirements.
- [x] (2026-09-07) Verify clean programme branch equals origin/main; no rebase
  needed.
- [x] (2026-09-07) Identify unmerged #11/#12 prerequisites and existing #21 PR
  #53.
- [x] (2026-09-07) Approve and dispatch detailed #40 and #41 plans to
  journeymen.
- [ ] P1: Land #40 and #41 independently; complete #21 diagnostic/recovery
  criteria.
- [ ] P2: Seed #42 boundary, then canonical contracts #43, admission #44
  and ports #45.
- [ ] P3: Extract provider execution #46 and session repository/recovery #47.
- [ ] P4: Deliver #49 read/task projections and #50 shared turn slice.
- [ ] P5: Complete remaining issue criteria and #42 dependency closure checks.
- [ ] Reconcile actual merged source trains and update #39 completion
  evidence.

## Surprises & discoveries

Main lacks versioned reads. #40 therefore starts at PR #12 head
`7863835542b91f4ab8a765346243dbfeda40a95e`, not main. #41's invalid global
cache is present on main and can be fixed independently. #21 already has PR
\#53 with direct-child policy and redacted parse diagnostics; its newer typed
planning extension needs reconciliation before further implementation.

PR #12's hosted logs show an ACP initializer missing `turn_diagnostics`,
Windows unused-parameter warnings, stale generated notices, a dependency
advisory, and other policy failures. These are current delivery prerequisites,
not #40 regressions. The full downloaded log is
`/tmp/vtcode-hex-pr12-ci-failures.log`; hosted run is 34050184721.

The latest baseline commit `6029f6f045ccdef86f9e08dfb66e0287927a76e2`
already scopes 13 of the 15 unwrap/expect findings. The remaining two are
PR12-specific and must be repaired in that train or its integration. Windows
warnings, the docs allowlist gap, the h2 0.4.15 advisory and stale notices
remain on both main and the latest baseline. Sequential gates may reuse the
shared build output at
`/home/leynos/Projects/VTCode.worktrees/hex-issue-40-read-metadata/target`
through `CARGO_TARGET_DIR`; do not change the shared Cargo registry cache.

PR11's eval gate fails on the same ACP missing-field initializer. Its coverage
gate also stops before tests because `cargo llvm-cov --no-run` produces no
profraw files; the latest baseline workflow uses the same command.

## Decision log

On 7 September root chose isolated #40 work above #12 and independent #41 work
above main. This follows #39's independence rules while preserving actual read
safeguards. Neither branch imports the unrelated lint train. The existing #21
PR will be extended or followed by a narrowly scoped layer rather than
duplicated.

User authorization overrides ExecPlans' per-plan approval gate. Repository
Conventional Commits override the commit-message skill's prefix preference;
file-based messages remain required. The user explicitly authorizes comments,
issue creation, readiness transitions and merges after the stated gates.

## Context and orientation

The programme worktree is
`/home/leynos/Projects/VTCode.worktrees/hexagonal-architecture-refactor`.
The issue worktrees are siblings `hex-issue-40-read-metadata` and
`hex-issue-41-provider-capabilities`; each holds its detailed plan under
`docs/execplans/` named after the final branch component. Context packs
`pk_klknubmr` and `pk_htwjeyir` carry their execution packets. Root retains
the global gate slot so two scrutineers cannot run suites in parallel.

## Verification plan

P1 requires production runner-history versions/continuations, actual snapshot
capability correctness across configuration/order/concurrency/model changes,
and classified planner failures with no duplicate children or uncertain replay.
Each issue's red-green regression is the negative control; a compile failure
does not count as the expected red assertion.

P2 requires schema/decoder cases proving policy and execution share the exact
canonical invocation; compile-fail/API tests proving callers cannot forge or
mutate admission; session/workspace/policy change invalidation and single-use
approval tests; and real runner/ACP consumers exercised with fake narrow
ports.
The seeded dependency/import check must fail for an injected forbidden edge.

P3 requires scripted provider streams and paused-time deadline/cancellation
tests, distinct retry ownership, and the same session repository contract suite
against memory and file adapters. Crash-point tests must cover pre-dispatch,
post-dispatch/pre-commit and committed outcomes; no uncertain effect can
replay.
Stale revisions and workspace ownership must reject conflicting writes.

P4 requires one set of canonical read/task fixtures projected to model,
terminal and ACP golden outputs. The same integrated turn application must run
through ACP and headless drivers for success, cancellation, approval denial,
partial provider failure, tool failure and hook refusal. Compare semantic
events and outcomes with explicit frontend capabilities, not timing or display
strings.

P5 checks actual Cargo dependency closure and imports, excluding concrete
frontend/protocol/HTTP/filesystem/process adapters from the migrated
application.
A forbidden-edge negative control must fail CI's check. Existing archive/wire
fixtures remain compatibility witnesses. External crate/runtime interfaces are
axioms; tests verify repository-owned interactions rather than third-party
internals. Detailed issue plans must state bounds and residual gaps.

## Milestones and delivery

P1–P5 above are programme stages; each issue receives its own detailed approved
ExecPlan before implementation. The early #42 seed and #49 contract slice are
separate from their later integration acceptance, avoiding circular blockers.
After each major issue milestone, record conformance, pass all applicable
deterministic gates, run `coderabbit review --agent`, address concerns and gate
the commit. Recovery uses scoped reverts or backed-up, lease-protected rebases;
never discard another agent's changes.

Publish narrow drafts using gh stack and complete issue/plan metadata. Fix CI
immediately, mark ready with leynos when green, check comenq before queueing to
avoid duplicates, then record ETA. Dispatch review findings to a journeyman;
answer every thread with `@coderabbitai`. Reconcile pre-merge tables against
the current code after inline concerns are settled. Merge as soon as applicable
nitpicks are addressed and checks are green. Do not demand an endless
perfection loop. Sync/rebase descendants after each bottom-up merge, checking
all conflicts and preserving both branches' intent.

## Active execution record

Issue40's corrected red run confirmed two reducer contract failures. Its
runner fixture still denied read_file: the tool is deliberately hidden from
AgentRunner's model catalogue, not missing from the behavior map. No new issue
is warranted. The supported file_operation route supplies runner-history and
guarded-patch integration coverage; the reducer tests supply the negative
control. Root authorized the narrow fix. Evidence is in
`/tmp/issue40-corrected-red-hex-issue-40-read-metadata.out`.

Issue41's real snapshot consumer has failed the intended tools-capability
assertion, recorded in `/tmp/issue41-red-vtcode.out`. Its library test had a
borrowed-boolean fixture error, now corrected. The consumer red is sufficient
negative-control evidence: root authorized the narrow global-cache removal,
now applied with three configured-router tests and the actual snapshot test
passing. Its obsolete cache-related lint expectation still needs removal.
Next are issue21's red regression and CI prerequisite gates, then queued
issue40/issue42 validation. Confirm slot ownership with root before any suite;
this queue is a snapshot.

The issue21 worktree is `hex-issue-21-planning-diagnostics` on existing branch
`fix/issue-21-subagent-planner-recovery`; its approved plan is
`docs/execplans/issue-21-subagent-planner-recovery.md`. A journeyman owns it.
The PR11 repair worktree is `hex-pr11-ci-repair` on existing branch
`fix/acp-custom-provider-routing` at `b231feaa9`. Its sole uncommitted code
change initializes optional checkpoint `turn_diagnostics` to `None`; an artisan
verified that ThreadSnapshot has no diagnostic value to supply. It still needs
full gates and root-coordinated publication before descendants can rebase.

The programme document passed scoped Markdown and whitespace gates and was
committed as `7c620bbf0`. No issue code has been committed, pushed or reviewed.

The CI prerequisite worktree is `hex-ci-prerequisites`, branch
`hex/ci-prerequisites`, with approved `docs/execplans/ci-prerequisites.md`.
It contains bounded coverage/Windows/h2/notices repairs and a byte-identical
move of the transient handoff to docs/archive, as required by repository
policy. The generator used pinned cargo-about 0.9.1. Gates remain pending.

The issue42 seed worktree is `hex-issue-42-boundary-seed`, branch
`hex/issue-42-boundary-seed`, with approved plan
`docs/execplans/issue-42-boundary-seed.md`. A journeyman is preparing the
protected application crate, existing request/response consumers and scoped
dependency enforcement while Wave 1 gates run. It cannot close full #42.

Always specify `--repo leynos/vtcode` in gh calls: no default is configured,
and an unqualified read selected upstream vinhnx/VTCode. Root corrected that
read before any mutation. Existing stacked PRs can show green evaluation
without full CI, which currently targets main; require actual integration
checks before merge.

## ACP priority and stack integration update

The user escalated missing gh access, MCP tools and skills in the Lody ACP
session, then requested verification that the installed release contains the
pending fixes. The installed wrapper invokes ~/.cargo/bin/vtcode with an
explicit ~/.vtcode/vtcode.toml; version 0.156.1 alone does not establish
source provenance. Read-only binary and MCP/skill discovery audits are
underway. No release replacement or configuration change has yet been made.

The command-policy probe could not link existing vtcode_core rlibs (E0463),
so its result is inconclusive, not a policy test pass. Exact logs and the
bounded hypothesis are in docs/debugging/debugging-plan-20260907-acp-gh-
policy.md. The session logs and source independently identify the command
allowlist gate.

Live GitHub stack inventory on 7 September 2026 identifies Stack #37 as the
other agent's exclusive train; do not rebase, relink or publish its
branches. Our existing ACP train is Stack #16 (11, 12, 13, 14, 17), followed
by three commits on feat/acp-responses-hardening without a PR, then Stack
\#57 (53, 55, 56). Local remote-tracking ancestry is linear with no merge
commits at each edge. Preserve the missing three-commit layer in the review
chain before joining the stacks. Refresh remote heads before any mutation.
Keep issues #35 and #36 logically separate even when their PRs share the
stack; no paid probes are authorized. Dependabot PR #78 is not our authored
work.

CI prerequisite cheap gates and notice freshness passed; expensive gates are
paused. The user now requires ACP linearization first, then tool/MCP/skills
resolution, then fast-forwarding main and origin/main to the latest merged
Stack #37 PR and rebasing the ACP stack. Bring ACP to green checks, review
equilibrium and merge before resuming hexagonal architecture work. Stack #16
is explicitly ours. All hexagonal worktrees are parked with uncommitted work
preserved. No main update or history rewrite has been performed. Issue21's
scaffold import error has been repaired, but its runtime red remains
unverified. Issue40, issue41 and issue42 await explicit slots. No CodeRabbit
review has been requested without successful deterministic gates.

## Outcomes & retrospective

Planning, branch setup, reconnaissance and the initial programme document
commit are complete. Both immediate defects have failing regression evidence;
three #41 router tests and its snapshot regression pass after the fix. Full
code gates and remaining integration evidence are pending. No new PR,
completed review or merged issue is claimed.

Revision note: Initial programme record captures live source-train constraints,
delegation, stage obligations and the first two issue handoffs.
