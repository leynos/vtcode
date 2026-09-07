# Deliver the hexagonal architecture remediation programme

Status: IN PROGRESS under the user's standing authorization to implement,
review and merge each issue on green without per-plan approval. Hexagonal
implementation is ON HOLD while the ACP hardening stack and its tool-surface
recovery are brought to review equilibrium.

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
The initial programme and ACP-stack work started from
`1aa90f196dacfb40ac96beb6e5aeafb7784a7d10`. Following the latest merged
Stack #37 layer, local `main` and `origin/main` now point to
`dddd1352fcdf41e86abe380423221f4b41f171e0`.

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
- [x] (2026-09-07) Verify the clean programme branch initially matched
  origin/main; that was the initial-base check, before the ACP integration
  rebase and the main fast-forward.
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
source provenance. The affected Lody agent configuration now has the additive
environment setting
`VTCODE_COMMANDS_ALLOW_LIST=gh pr view,gh pr ready,gh --version`. It applies
to a fresh ACP process; the original denial remains valid for the already
running session.

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

Some CI prerequisite cheap gates and notice freshness passed; the pinned
Markdownlint zero-file check was a false green and its full-baseline repair is
tracked separately. Expensive gates are paused. The user required ACP
linearization first, then tool/MCP/skills
resolution, then fast-forwarding main and origin/main to the latest merged
Stack #37 PR and rebasing the ACP stack. The main fast-forward is complete at
`dddd1352`; the ACP cascade is complete locally. Bring ACP to green checks,
review equilibrium and merge before resuming hexagonal architecture work.
Stack #16 is explicitly ours. All hexagonal worktrees remain parked with
uncommitted work preserved. No paid provider calls, GitHub mutation from the
release probe, or binary reinstall has been performed. Issue21's scaffold
import error has been repaired, but its runtime red remains unverified.
Issue40, issue41 and issue42 await explicit slots. No CodeRabbit review has
been requested without successful deterministic gates.

## Outcomes & retrospective

Planning, branch setup, reconnaissance and the initial programme document
commit are complete. Both immediate defects have failing regression evidence;
three #41 router tests and its snapshot regression pass after the fix. Full
code gates and remaining integration evidence are pending. This retrospective
describes the initial programme state; the later ACP stack update records the
current rebase and release evidence.

Revision note: Initial programme record captures live source-train constraints,
delegation, stage obligations and the first two issue handoffs.

## ACP stack rebase milestone (7 September 2026)

Hexagonal implementation remains paused. Eight focused ACP tool-surface tests
are green: four core tests in
`/tmp/acp-tool-surface-green-core-final.out` and four ACP tests in
`/tmp/acp-tool-surface-green-acp-rerun-5.out`. Full gates and executable release
verification remain pending. One implementation journeyman subsequently hit
an account usage limit; the scrutineer remains available.

Root verified that PR #24 is still the latest merged layer in Stack #37 and
fast-forwarded local main and origin/main from 1aa90f196 to
`dddd1352fcdf41e86abe380423221f4b41f171e0`. GitHub readback agrees; the main
worktree was clean before and after. No open Stack #37 branch was changed.

### Rebase ownership and recovery state

A journeyman completed the local cascading rebase in
`/home/leynos/Projects/VTCode.worktrees/acp-stack-integration`. Root owns
publication, saved-WIP restoration, release installation and final merges.
Use artisans for bounded individual conflicts, wyverns for source/history
questions and scrutineers for sequential syntax and final gates. Always use
absolute paths with apply_patch: its relative base is the root conversation
worktree, not an exec command's working directory.

Native gh-stack tracking now adopts these ten branches, bottom to top:
11, 12, 13, 14, 17, 103, 104, 53, 55, 56. Recovery references are
`refs/archive/acp-linearized-20260907/pr-N` for each PR. The immutable initial
heads and bases are recorded in
`/tmp/vtcode-acp-stack-before-rebase-20260907.json`.

Saved WIP records are in `/tmp/vtcode-acp-rebase-wip-20260907.json`:

- ebcc73bc1fc6caa2101465cfe774661d5468adee: PR11 initializer repair.
- 08db8c85e6a22b6e99495e38e8b66ea9d38446d3: PR13 review plan and initial
  atomic-thread-mutation API/test. This patch was relocated from an accidental
  root-worktree edit; root is otherwise free of implementation edits.
- ce1547c6904fbf953b4d977bb0eaa67b461b6c50: the latest tool-surface repair,
  plan and offline executable smoke fixture. Its installed-binary smoke has
  now run RED; candidate binary verification remains pending. It supersedes
  feb450f4a38c5327bfab24622af26bb57fd15e4b, which remains retained for
  recovery; the superseded stash's smoke was not executed.

The parked #21 architecture diagnostics remain dirty and untouched on local
branch `hex/issue-21-diagnostics-parked`; do not include them in the PR53
rebase.
Other occupied ACP branch worktrees were detached before adopting the stack.
Do not apply/drop stashes or change those worktrees during this milestone.

### Historical conflict strategy and initial stop

Read rebase, github-stacks, sem and weave-git-merge skills and repository
instructions. Root inspected Weave's preview through the reconnaissance agent.
Use the built-in merge path for this cascading import/API rename replay by
passing the following environment to every gh-stack rebase/continue call:

    GH_REPO=leynos/vtcode GIT_CONFIG_COUNT=2
    GIT_CONFIG_KEY_0=remote.origin.gh-resolved GIT_CONFIG_VALUE_0=base
    GIT_CONFIG_KEY_1=core.attributesFile GIT_CONFIG_VALUE_1=/dev/null

This is a per-command override, not a global config edit. Attribute readback
confirmed merge is unspecified for the affected Rust files with this override.
Use `gh stack rebase --continue` after staging each resolved conflict; never
start a competing plain rebase or silently abort the current gh-stack state.
Do not push until root has reviewed the result and applicable gates are green.

The initial stop replayed d2c39f961 in PR11. Root inspected all three index
stages
of `vtcode-llm/src/providers/openai/provider.rs`, retained the merged
`model_behaviour` spelling and added `custom_provider_config` field/defaults.
The working file is resolved but not yet staged. Scrutineer syntax validation
and diff-check passed; stdout is
`/tmp/acp-rebase-openai-provider-syntax.rs`, stderr is the adjacent `.err` file.
The file was then staged and the cascade continued. This is historical context,
not the current rebase stop.

For every conflict, inspect index stages and original commit intent before
editing. Preserve ACP runtime behavior and merged analyse/artefact/behaviour
names, including harness_artefacts.rs. Do not resurrect deleted synthetic plan
progress solely for a spelling rename. Preserve d57491acb's bounded registry
fixture synchronization alongside later tests. Do not mechanically replace
wire/JSON fields, legacy aliases or external API names.

After each conflict, ask a scrutineer to syntax-check affected files without
modifying tracked files and capture /tmp logs. All gates are globally serial;
no other gate is active at handoff. Final acceptance requires no unmerged
index entries, linear ancestry from new main through all ten layers, bounded
range-diff/semantic review, and final applicable deterministic gates after
root restores lower-layer repairs. No CodeRabbit request before those gates.
Report the completed old-to-new SHA map and any unresolved semantic concerns.

Root subsequently restored the tool-surface stash onto its unchanged b928ed34
worktree to let an artisan correct the smoke harness's ACP result-envelope
validation. That worktree is dirty again and is not in the ten-layer native
stack. Save its latest state again before rebasing that additional top branch.

The harness envelope correction is now saved with the full tool-surface WIP
in `ce1547c6904fbf953b4d977bb0eaa67b461b6c50`, superseding feb450f4 for
restoration. The old stash is retained for recovery. The tool-surface worktree
is clean again. CI prerequisites separately advanced to dddd1352 and retained
the new baseline handoff bytes during archival; their gates remain pending.

### Completed rebase and repair handoff

The ten-layer local cascade is complete. Its base is dddd1352 and its top is
8e436821. Intermediate heads in PR order are 654378e0 (#11), 272f0883 (#12),
399370bd (#13), 470d3e19 (#14), 57162a56 (#17), 493e376e (#103),
6527596a (#104), d9ca178d (#53) and 637439de (#55). The integration worktree
is clean, has no unmerged index entries or merge commits in this range, and
native stack tracking reports no layer needing a rebase. Exact heads are in
`/tmp/acp-stack-rebase-local-stack-view.json`; the reviewed range-diff is in
`/tmp/acp-stack-rebase-range-diff.out`. All conflict syntax checks passed.
These are local integration results; the rewritten branches are not pushed.

Root restored the one-field PR11 checkpoint initializer repair onto 654378e0
in its dedicated worktree. The PR13 journeyman owns restoration onto 399370bd
and prepares the approved review regressions with artisans. The stack-rebase
journeyman now owns restoration of ce1547c onto a separate tool-surface branch
based on 8e436821, followed by its existing source and offline release plan.
Saved stashes remain available. No parked hexagonal patch is included.

PR13 review repair completed its positive-control and fault-injection gates
through the shared scrutineer, then released the sequential gate slot. Other
journeymen prepare bounded changes without running tests, formatters or
linters until the slot is handed over. The
separate CI repair is intended to become the bottom layer of Stack #16 after
validation; root must verify the full ordered `gh stack link` update and
cascade the remaining layers again. PR11 and PR13 repairs likewise require
gating and propagation before publication. Do not treat the present linear
topology as evidence that these pending repairs have been integrated or
reviewed.

The installed binary was rechecked on 7 September: SHA-256 remains
0450179e27b5c011cd923fed23b763ab9b241aa2941d338eac39ce572a39d80d and Build ID
d810a3c724d1e2a0b897f52f26837d5b44eae02c. An installed-binary ACP smoke run
is RED at `/tmp/acp-tool-surface-installed-red-1788781135/run-3535276`. It
advertised exactly `read_file`, `list_files`, `apply_patch`, `exec_command`,
`write_stdin`, `agent` and `task_tracker`; the three skill tools and mock MCP
surface were absent. This is direct evidence that the installed binary does
not expose the required tools; its exact source SHA remains unknown. No
replacement was installed. Local main and origin/main
both point to dddd1352.

The PR11 inherited-rebase compatibility repair is prepared in four files but
is uncommitted. PR13 has a valid archive-attachment RED and a passing atomic
message test. Its real-client positive control now passes, and both streaming
and buffered write-ahead tests fail on the observed filesystem mutation after
a failed checkpoint. The decisive logs are
`/tmp/acp-pr13-write-ahead-positive-control.out` and
`/tmp/acp-pr13-focused-red-write-ahead-real-client.out`. The journeyman has
released the gate slot and is dispatching the approved production repairs.

Revision note: records the completed local cascade, restored repair ownership
and exclusive gate handoff. Remaining work is deterministic validation,
lower-layer repair propagation, executable release verification, review and
bottom-up merge; hexagonal implementation remains paused.

### Installed ACP release accepted

Later on 7 September, root completed the installed-release acceptance. This
supersedes the earlier installed-binary RED and no-install status above.
The isolated integration candidate is clean at commit
`30b348853b7f934f8ce004e48dc1f5b87b336cd3`, with tree
`ad62497332da7216845798d703f8785afee2605a`. It includes the separate CI,
PR11, PR13 and tool-surface repairs, canonical MCP routing, and the subsequent
MCP allowlist composition repair. It is an integration artifact, not an
aggregate PR or evidence that the narrow source branches have been published.

The final Rust suite passed 10,419 tests with 30 skipped; the harness and
documentation gates passed. CodeRabbit reviewed the final eight-file delta
with zero findings after deterministic gates. Root atomically installed the
release artifact at `/home/leynos/.cargo/bin/vtcode`. Its SHA-256 is
`2dc544f8ec5eba14544e13da77380f53123f506eb4e9289c00ec73dc279c04ed`;
the previous executable remains in the release-artifacts rollback directory.
Both artifact and installed-path offline smoke tests passed: all three skill
operations, exactly one MCP echo call, successful `gh --version`, and the
final ACP marker. Existing processes were not restarted by installation.

Evidence is retained in
`/tmp/acp-installed-release-provenance-policy-final.txt`,
`/tmp/acp-installed-release-install-record-20260907.json` and
`/tmp/acp-installed-release-policy-installed-smoke-final.out`.
The built candidate's tracked plan remains unchanged after its build; these
external records preserve the exact source-to-artifact relationship.

Operator configuration was repaired separately. Four Lody ACP configurations
now include the requested local comenq, GitHub comment, issue and PR creation
command grants; the issue-comment API grant is scoped to that endpoint.
No SSH allowance was added. Exact registered MCP tool grants were added to
the global policy and the reported rstest-bdd and Cuprum workspace policies,
with rollback copies and field-preserving readback checks. Fresh processes
are required to inherit the command environment. These configuration edits
do not imply that every existing workspace policy has been migrated.

The user-requested `~/.local/bin/vtcode_for_lody` helper was installed outside
the repository after formatting, lint, syntax and eleven mocked protocol
tests passed. Its default is read-only status; `--terminate` uses Lody's
session-specific local RPC after verifying the agent configuration and daemon
identity. Root used that RPC for the requested Cuprum session and received
an exact-session success response. No PID is guessed by the helper.

Issue #109 separately tracks primary-agent inline MCP providers omitted by
ACP startup. Unified/full-auto callers do merge them. This is a static
follow-up finding, distinct from the repaired allowlist composition and
PR11's partial-initialization fixes; its offline acceptance remains pending.

The serial gate slot now belongs to the combined fifteen-path CI foundation
in `hex-ci-prerequisites`. It incorporates the reviewed #108 maintenance
ratchet and the earlier CI repairs so the bottom PR can pass independently;
issues #107 and #108 remain open debt. The ACP lead prepares narrow replay
packets without running gates concurrently. Canonical MCP routing belongs
with tool-surface recovery; allowlist composition remains a separate narrow
follow-up. Exclude the candidate-only integration plan from those PRs.
Publication, hosted CI, queued reviews and bottom-up merges remain pending.
Hexagonal implementation stays paused until the ACP stack has converged.

### ACP behavioural acceptance expanded after live incident

The user's 7 September continuation makes five ACP contracts explicit:
honour the permission-skip flag and enforce permissions without it; advertise
and load the skill menu; keep skills usable; preserve MCP functionality; and
return accurate, bounded file windows with truthful position, length and
continuation metadata. Completion requires unit, behavioural, property and
data-contract tests, executable ACP scenarios with retained session recordings,
and Vidaimock timing/failure scenarios. These requirements supersede treating
the earlier fixture-only smoke as sufficient installed-session acceptance.

The installed executable still hashes to
`2dc544f8ec5eba14544e13da77380f53123f506eb4e9289c00ec73dc279c04ed`.
It was installed at 17:31:51 UTC. Lody logs show the reported rstest-bdd session
`16f610b6-3473-45d9-9611-fb19d007f54e` starting at 17:42:10 UTC, after that
installation. Its skill failures cannot be dismissed solely as an old process.
The previous smoke proved a fixture skill and an explicitly allowed command;
it did not prove real home skills or the requested skip-flag behaviour.

Read-only tracing identified invalid frontmatter in the installed `rebase`
and `python-testing` skills, and a separate primary-agent filter that can omit
skill tools. A fixed offline provider sequence now probes real home skill
loads and direct reads through the installed binary, without executing skill
instructions or calling paid providers. The retained recording in
`/tmp/acp-real-home-skills-probe/run-1390512/probe-report.json` proves
`pr-creation` and `rust-router` load, both malformed skills fail, and direct
home-file reads are rejected. Local frontmatter repairs preserve instruction
bodies byte for byte; upstream reports are agent-helper-scripts #116 and
python-skill #4. The post-repair executable recording in
`/tmp/acp-real-home-skills-probe-fixed/run-1526860/probe-report.json` proves
all four skill loads succeed. Direct home-file reads remain rejected; this
does not establish resumed-session or restricted-agent skill access.

Issue #111 tracks ACP read-window normalization: core-style offset aliases
are silently dropped by the ACP route. The original report lacks raw tool
arguments, so source evidence establishes the dropped-alias defect without
claiming it explains every display discrepancy. A separate journeyman owns
the bounded read repair and property/behavioural regression coverage.

For permission acceptance, ordinary commands must work with the skip flag
without individual default allowlist grants; the inverse path must prove
enforcement with no side effect. Explicit configured denies remain effective.
The permission journeyman owns runtime propagation and its regression tests
in a new worktree based on the immutable installed candidate. No candidate
files or existing stack heads may be overwritten by these investigations.

The CI foundation is now draft PR #110. Local gates and CLI review passed,
but hosted checks exposed further Windows and notice-generation failures.
The CI lead owns those repairs. Root coordinates one scrutineer gate slot
across all teams. Context Pack creation failed on an oversized unrelated
stored pack; exact commit/path handoffs are the temporary fallback.

At the user's request, an Astra agent owns two separate spycatcher-harness
draft PRs: roadmap/ADR for a strict cassette-backed ACP file-read slice, and
an RFC for broader ACP act/expect control with cassette inference. It works
in separate repository worktrees and shares the serial validation slot.

Revision note: expanded ACP acceptance to the user's five functional contracts
and required verification layers; recorded the installed-session counterexample
and delegated independent permission and file-read repairs. Skill and MCP
integration verification, source-stack publication and green review/merge
remain outstanding. Hexagonal work remains on hold.
