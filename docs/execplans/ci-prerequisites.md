# Restore shared CI prerequisites for the remediation stack

Status: APPROVED by root under the user's standing programme authorization.

## Purpose / big picture

Remove verified shared CI failures before merging issue #39's remediation
stack. Keep platform warnings, coverage execution and dependency freshness
repairs out of the immediate read metadata and provider capability fixes.
Preserve the baseline train's existing lint fixes and the capability train's
runtime safeguards. This plan does not authorize blanket warning suppression
or weakening dependency/security checks.

## Conformance basis and constraints

The user requires green deterministic gates before CodeRabbit reviews and
green hosted checks before merge. The current merged baseline is
`dddd1352fcdf41e86abe380423221f4b41f171e0`; the worktree is
`/home/leynos/Projects/VTCode.worktrees/hex-ci-prerequisites`, branch
`hex/ci-prerequisites`. The prior `6029f6f04` reference is an unmerged
Stack37 branch and is not copied into this work; all current reconciliation
uses merged baseline `dddd1352`. The merged baseline contains the failures
described below. The separate PR11 checkpoint fix
belongs to its existing branch and must not be copied into this worktree.

Preserve APIs, permission behavior, release invariants and MSRV. Use existing
source generators for generated output. Do not introduce independent Cargo
registry caches. Do not use /tmp as a build target. Other agents own other
worktrees; never revert their edits or stop their processes. No paid probes.
Use British English with Oxford spelling in authored prose.

## Tolerances and ownership

The journeyman owns this worktree's implementation and plan updates. Delegate
bounded mechanical changes to artisans and read-only exploration to wyvern.
Only scrutineer runs gates, after root grants the global sequential slot.
Root owns publication, external comments, stack integration and merges.

Allowed scope: the two Windows parameter warnings in
`crates/common/vtcode-commons/src/vtcode_paths.rs`, coverage workflow and its
directly relevant tests, the top-level documentation location repair, the
smallest compatible h2 lockfile update, generated THIRD-PARTY-NOTICES and this
plan. Root additionally approved `.github/workflows/ci.yml` plus a small Node
helper and its contract test for safe incremental changed-Markdown linting.
The reviewed issue #108 ratchet is now combined with this component because
neither prerequisite can satisfy hosted CI alone. The combined CI-foundation
source has exactly fifteen paths: these ten paths plus issue #108's baseline,
checker, checker test, guide and plan, with `.github/workflows/ci.yml` shared.
There are no API changes, unrelated dependency updates or new production
dependencies. Escalate additional failures with exact evidence before editing
outside this scope. Never add blanket cargo-unmaintained or unwrap/expect
exemptions to obtain green checks.

Root authorised one inherited gate-blocker repair after actionlint identified
two unused retry-loop counters in `.github/workflows/build-linux-windows.yml`.
Changing each `for i` to `for _` preserves the twelve-attempt loop behaviour
without expanding the workflow's runtime policy. The combined source has seven
tracked modifications, one archive rename and seven untracked additions: the
approved fifteen paths.

## Progress

- [x] (2026-09-07) Compare failures with both live source trains.
- [x] (2026-09-07) Create isolated main-based prerequisite branch.
- [x] (2026-09-07) Validate and implement the bounded CI repairs.
- [x] (2026-09-07) Preserve WIP in named stash
  `bf5e5bb1c910d73b1662a4c7468c8a984d77d390`, fast-forward to merged
  baseline `dddd1352fcdf41e86abe380423221f4b41f171e0`, and reapply repairs.
- [x] (2026-09-07) Prove changed-Markdown helper contracts and pinned CLI
  selection; pause before long standalone gates for release integration.
- [x] (2026-09-07) Supply the frozen CI component patch to the combined ACP
  release candidate; candidate commit
  `21b24eaeacac7c5595477ebf5da94b9c299121b3` applied it unchanged before its
  clean review. Propagate only this component's candidate rustfmt hunk back
  here; retain this branch's independent gate and review milestone.
- [x] (2026-09-07) Record replayable pre-combination backup
  `/tmp/ci-foundation-precombine-20260907.patch` (SHA-256
  `563598f67a0f6eb73a3ea5f4f11cda86200c0f9fd59ccb56440dad72fadb2a41`),
  verify and apply reviewed issue #108 commit
  `0b51c9ba1bf2b09b74f55201e06c882fb3589894` from its recovery patch.
- [x] (2026-09-07) Run the final combined fifteen-path deterministic packet
  through the granted serial scrutineer slot. Every applicable local gate is
  green; record external host limitations separately.
- [x] P1: CodeRabbit reviewed the staged combined source after all applicable
  deterministic gates; it reported zero findings in
  `/tmp/coderabbit-VTCode-hex-ci-prerequisites-2.out`.
- [x] P2a: Commit and publish draft PR
  [#110](https://github.com/leynos/vtcode/pull/110).
- [ ] P2b: Repair applicable hosted failures, reach external review
  equilibrium and merge on green.

## Surprises & discoveries

PR11 evaluation fails compilation because its ACP checkpoint initializer
omits optional turn_diagnostics. Root already prepared that one-field repair
in the separate PR11 worktree. It is not a failure on this main-based branch.

PR11 coverage fails before running tests: cargo llvm-cov --no-run attempts to
merge absent profraw data. The latest baseline still uses that command. The
Windows native_roots home_dir parameter and non-Unix set_private_permissions
path parameter produce deny-warnings failures on Windows. The stable handoff
document remains outside scripts/docs_top_level_allowlist.txt.

The documentation-location invariant classifies the tracked baseline handoff
as a transient checkpoint report, rather than a stable top-level entrypoint.
The plan therefore moves it byte-identically to `docs/archive/`, without
adding an exception to the top-level allowlist. Repository search found no
current-tree references; commit-pinned historical URLs intentionally remain
valid at their source revisions.

The lockfile pins h2 0.4.15; CI identifies RUSTSEC-2026-0258 and a fixed version
of at least 0.4.16. Verify the current advisory and compatible resolution before
updating. Licence generation reports stale notices. Most reported production
expect findings are already scoped in the separate baseline train; do not
duplicate or broaden those exceptions. Two PR12-specific expectations require
separate reconciliation after the baseline is integrated.

The upstream EmbarkStudios cargo-about 0.9.1 release supplies an
x86_64-unknown-linux-musl archive and SHA-256 sidecar. Its published checksum
verified before generation. A first command was rejected before execution
because its cleanup trap used `rm -rf`; the replacement used a retained
`mktemp` scratch directory and the verified archive directly. It did not
compile or install a Cargo tool.

The available cargo-audit was 0.22.1 while CI pins 0.22.2. The matching
official RustSec 0.22.2 x86_64-unknown-linux-gnu archive was downloaded to
scratch and verified against the release API SHA-256, ready for the deferred
locked advisory gate without a competing Cargo build.

The merged baseline changed `docs/rust-baseline-handoff.md` after the parked
patch was prepared. Applying the stash without index restored every narrow
change; the archive destination was then overwritten from merged baseline
`dddd1352` before staging the approved rename. The source and archived file
hashes are both `715cb9f0615630dcb8aa258e1c95df21ab618efcf30c1975e75582d06df7b194`,
so relocation preserves the actual integration-base bytes.

The CI-pinned cargo-unmaintained 1.10.0 has no upstream binary release asset.
Signed binstall lookup also lacks a QuickInstall signature and can only fall
back to a source build. Its installation and the required
`cargo unmaintained --no-warnings` evidence therefore wait for the shared
slot; no exclusions or warning suppression are proposed.

The first current-base deterministic packet passed disk capacity (558G free,
67% used), diff, documentation-location/link, workflow-security and actionlint
checks. It then stopped at repository-wide `markdownlint-cli2 '**/*.md'
'!target'` with 23,054 existing style errors across 353 files; log
`/tmp/ci-prerequisites-markdownlint-hex-ci-prerequisites.out`. This exceeds
the twelve-file repair budget and does not identify a changed prerequisite
file. Root recorded the inherited full-repository blocker as GitHub issue #107
for Stack37 coordination, with no suppression or duplicate issue. The one
introduced plan-line MD013 finding was wrapped in scope. Generated/dependency,
Makefile, Windows, coverage and CodeRabbit checks remain unrun pending the
shared slot after ACP RED work.

The initial local scan used markdownlint-cli2 0.22.1. The pinned hosted
markdownlint-cli2 action resolves CLI 0.23.2 and passes each newline-separated
workflow glob literally; the current YAML block includes quote characters. The
next slot must run both faithful literal-glob 0.23.2 and normalised intended
glob checks, treating a zero-file literal scan as false green. No workflow
change is authorised until that evidence distinguishes a baseline issue from a
configuration defect.

The approved follow-up uses paths-filter JSON into a fixed Node helper, then
markdownlint-cli2 0.23.2 `--` literal arguments through a no-shell spawn. The
action's comma-separated changed-files example remains unsuitable because
commas delimit entries.

The helper negative control rejects an empty JSON array with its required
diagnostic. Seven Node contracts passed, real pinned CLI 0.23.2 lint passed for
the two changed documents, and an existing invalid Markdown file produced the
expected 58-finding failure. Actionlint and diff checks also passed. The helper
wrapper's own matcher returned one for the correct empty-selection error; that
is orchestration evidence, not a helper defect, and it was not rerun.

The final frozen component patch has SHA-256
`4e61ab6a4a44716de9f8b2b3688c53133ab0659d28bd04a0d607a17f3df3e3cd`.
The ACP release candidate applied it unchanged and reached reviewed commit
`21b24eaeacac7c5595477ebf5da94b9c299121b3`. That candidate also contains ACP
recovery work, which remains outside this PR. Its formatter changed the layout
of this component's two Rust allowance attributes, so this branch adopts only
that no-semantic-difference hunk for a later clean standalone gate. The
reviewed issue #108 source has since joined this CI-foundation branch.

The reviewed issue #108 ratchet passed its own historical branch gates, but
this component still leaves raw cargo-unmaintained findings failing in hosted
CI. Conversely, #108 alone leaves this component's h2 advisory, Windows
warnings and CI workflow failures. Root therefore selected one combined
CI-foundation source instead of two merge-ordered prerequisites. Its exact
recovery patch is `/tmp/issue108-baseline-ratchet-0b51c9ba.patch` with SHA-256
`ddf549689a42b8fa2ca1e9ef97355fd0bff0b5ecdff3ff1e403cd626b19be02a`;
`git apply --check` succeeded before replay. The source applies no behaviour
beyond the two already reviewed components.

The final combined deterministic packet is green. It passed the pinned
changed-Markdown helper and seven Node contracts, thirteen #108 checker tests,
location/link/workflow/actionlint checks, notices with verified cargo-about
0.9.1, the locked audit with verified cargo-audit 0.22.2, and the live
28-identity scanner ratchet. `make -j1 typecheck`, `make -j1 check-fmt`, and
the full native Make-equivalent passed with warnings denied; the workspace ran
10,090 tests with 17 skips and required harness suites passed. The dedicated
vtcode-commons coverage witness ran 359 tests and emitted a 403,998-byte LCOV
file. Logs are under `/tmp/*-VTCode-hex-ci-prerequisites.out`.

## Decision log

Root selected one main-based CI-foundation PR because these defects predate and
affect multiple issue branches, and the reviewed #108 ratchet is required for
the component's hosted CI to pass. Scope is limited to the fifteen verified
combined paths. Existing baseline and capability fixes retain their own
delivery provenance. Plans are approved under the user's authorization;
workers do not request per-milestone user approval.

Root approved the narrow documentation adjustment after the location invariant
required a move rather than an allowlist exception. The file is moved
byte-identically to `docs/archive/rust-baseline-handoff.md`; no current-tree
link needs rewriting.

After root fast-forwarded `main` to merged baseline `dddd1352`, this branch
fast-forwarded from `1aa90f1` and reapplied its WIP from named stash
`bf5e5bb1c910d73b1662a4c7468c8a984d77d390`. The source relocation follows
the merged document rather than the obsolete pre-fast-forward content.

Root superseded the separate issue #108 prerequisite PR shape with this focused
CI-foundation PR. The #108 commit and branch remain recovery and review
provenance. Its five unique paths join this component's ten paths; the shared
CI workflow makes fifteen paths in the combined source. The issue's baseline
debt remains open and visible; this decision changes delivery ordering only.

Root approved a narrow changed-Markdown CI ratchet after the exact pinned
0.23.2 witness showed current quote-bearing workflow arguments select zero
files and exit successfully, while normalised globs select 353 files and fail.
The replacement is JSON paths-filter output consumed by a static Node helper,
which passes verified added/modified Markdown files as literal CLI arguments.
Full-repository debt remains issue #107; no rule suppression is allowed.

The workflow retains only checkout and pinned paths-filter for this job; the
markdownlint action was removed because, without explicit globs, it defaults to
a broad root scan and duplicates the helper. The helper invokes exactly
`npx --yes --package markdownlint-cli2@0.23.2 markdownlint-cli2 -- <paths>`
through `spawnSync` with `shell: false`; its test-only command override exists
solely to capture and assert that production argv contract.

A separately retained release-integration source snapshot includes this full
tracked-and-untracked patch and manifest. At that snapshot, the long standalone
CI gates and CodeRabbit review were still pending this branch's own milestone.

Candidate integration demonstrates that the frozen CI component applies with
the ACP recovery candidate. It is not substitute evidence for this branch's
own final-source checks, generated-notice/advisory/unmaintained evidence,
Makefile, Windows and coverage gates, or its CodeRabbit review.

The Windows allowances now use the exact unused predicates: home_dir is
allowed only when the macOS branch cannot consume it, and the permissions path
only when the Unix body cannot consume it. This preserves behaviour without a
general warning exemption.

## Context and orientation

Read root AGENTS.md, affected crate AGENTS.md, relevant Rust routing skills,
and the existing workflow/checker sources before editing. Read context pack
pk_c7dxboru for CI reconciliation. Exact logs are
/tmp/vtcode-hex-pr12-ci-failures.log,
/tmp/vtcode-hex-pr11-eval-failures.log and
/tmp/vtcode-hex-pr11-coverage-failures.log. Paths in packs may resolve against
the root worktree; tag any supplied snippets with this worktree and its HEAD.
Use context_pack for worker code handoffs. CodeGraph has timed out repeatedly;
use known-path reads and available semantic navigation rather than repeatedly
waiting on the unavailable index.

## Plan of work

P1 starts with a bounded current-source check of every reported failure.
Determine whether the Windows arguments can be scoped to their platform
without changing behavior. Correct only those warnings; retain runtime
invariant assertions. Update the documentation allowlist only if the handoff
document is intentionally a stable top-level exception under existing policy.

Read official cargo-llvm-cov guidance for a nextest-compatible coverage run.
Replace the invalid pre-report step with the smallest supported workflow that
actually executes tests and emits the existing lcov artifact. Install nextest
if the workflow requires it. Preserve pinned action revisions, permissions,
triggers and Codecov failure behavior. Do not use cargo test. Add a narrowly
useful workflow contract check only if an existing script-test pattern fits.

Verify the h2 advisory and apply the minimum compatible locked update. Avoid
unrelated package churn; inspect the resulting dependency diff. Regenerate
licence notices using the repository's actual generator and pinned tool
version after the dependency change. Do not edit generated notice content by
hand. Preserve source inputs and check reproducibility.

After integrating the bounded repairs, record their conformance and run all
applicable deterministic commit gates. Fix supported in-scope defects and
report unrelated inherited failures to root. Only after gates pass, instruct
scrutineer to run coderabbit review --agent. Clear all applicable concerns,
rerun gates after fixes and commit with a Conventional Commit message from a
temporary message file. Publish the resulting narrow draft, while root owns
stack insertion and merge coordination.

## Verification plan

Use the repository scripts/Makefile targets where applicable. Run gates
sequentially through scrutineer with tee logs under /tmp and pipefail. Verify
nextest is installed before any script that otherwise falls back to cargo
test. Request the global slot before starting any format, lint or test job.
Use the shared build output directory
`/home/leynos/Projects/VTCode.worktrees/hex-issue-40-read-metadata/target` via
CARGO_TARGET_DIR, preserving default CARGO_HOME and registry cache.

Minimum evidence: scoped Markdown/workflow/security checks, generated-notice
freshness and advisory checks, all required code commit gates, locked Cargo
checking with warnings denied, and the Windows target check if the configured
toolchain supports it. The actual main Makefile is the required full
release/PR route: run `make -j1 typecheck`, then `make -j1 check`, with
`RUSTFLAGS=-D warnings` and the shared target directory. This replaces the
legacy script's duplicate, partly unlocked sequence; it does not waive any
gate. Do not claim hosted Windows coverage from Linux-only checks. Record any
environment prerequisite precisely rather than silently skipping a gate.

Coverage's negative control is the archived hosted failure before any test
execution. The corrected path must execute instrumented nextest tests and
produce non-empty lcov data. A textual command replacement alone does not
prove the coverage workflow. The fixed advisory must disappear from the
resolved lockfile check without suppressing it. Licence regeneration must be
idempotent. Windows compilation is the witness for platform warning removal.

Changed-Markdown validation first runs `node --test
scripts/tests/lint_changed_markdown.test.mjs`. Its contract covers JSON parsing,
non-empty input, literal `--` argv construction, hostile filenames, preserved
child status, regular-file checks and realpath containment. A focused helper
run then uses the actual pinned CLI 0.23.2 against each new or modified
Markdown path; changed Markdown must be green. This is incremental coverage,
not evidence that the baseline full-repository scan tracked by issue #107 is
green.

CLI review rate limits require vsleep for a randomly selected 45–90 minutes
before retrying, as instructed by the user. Report the wait to root so other
authorized independent work can continue. No major milestone advances with
unresolved applicable findings or failing deterministic gates.

The combined source now owns the serial slot. The optional ast-grep executable
at `/home/leynos/.local/bin/ast-grep` remains mode 0600 and must not be invoked
or bypassed. The reviewed release candidate also established that this Linux
host cannot compile aws-lc-sys for x86_64-pc-windows-msvc; record that host
cross-toolchain limitation rather than treating Windows validation as passed.

The ast-grep and Windows constraints remained unchanged during the final
packet: ast-grep was not invoked, hidden or bypassed, and Windows is not
reported green. Root's one-file plan maintenance check also passed the pinned
Markdown CLI and `git diff --check`, with evidence in
`/tmp/mini-{markdownlint,diff-check}-hexagonal-architecture-refactor.out`.

CodeRabbit reviewed the exact staged combined source after the final
documentation recheck and returned zero findings. Its transcript is
`/tmp/coderabbit-VTCode-hex-ci-prerequisites-2.out`; no review repair or gate
rerun is required before commit.

## Recovery and residual boundaries

Root handles conflicts, branch ancestry and lease-protected publication.
Workers must preserve unrelated staged/unstaged changes. If a dependency
update forces broader API/MSRV changes, stop that subtask and give root exact
evidence and alternatives. Do not weaken checks to conceal inherited issues.
Baseline import/expect ratchets and PR12 patch-guard details are separate
integration work. This PR does not claim to complete architectural issues.

## Outcomes & retrospective

Bounded implementation is complete: Windows-only unused-parameter allowances
preserve their platform branches; coverage now invokes instrumented nextest;
the lockfile pins h2 0.4.16; the transient handoff was archived byte-identically.
THIRD-PARTY-NOTICES was regenerated with verified cargo-about 0.9.1 output.
The frozen component was applied unchanged to clean reviewed ACP candidate
`21b24eaeacac7c5595477ebf5da94b9c299121b3`; only its matching Rust attribute
layout is retained here. The reviewed #108 ratchet now joins it as one
fifteen-path CI-foundation source. Initial scoped evidence passed: diff checks,
changed-Markdown helper contracts and selected-file lint, documentation
location/links, workflow security, and corrected actionlint. Commit
`62548b081afb01014e787a812a504e4ab8a2eec6` and draft PR
[#110](https://github.com/leynos/vtcode/pull/110) now carry the source. Hosted
CI, external review equilibrium and merge remain pending. Windows remains an
explicit host cross-toolchain limitation rather than a passing result. The
final staged CodeRabbit review returned zero findings.

Revision note: Initial approved packet separates shared CI repairs from the
Wave 1 runtime defects and records exact negative controls and scope limits.
