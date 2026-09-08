# Enforce the classified dependency maintenance baseline

Status: APPROVED by the parent execution lead under the user's autonomous
ACP hardening and green-CI remit. This prerequisite addresses issue #108.

## Purpose and constraints

Keep dependency maintenance findings visible while preventing new findings
from entering the ACP stack. The pinned scanner currently fails on 27 existing
packages. Most are transitive; its heuristics combine stale dependency
requirements and absent manifests in current upstream repository layouts.
Neither category alone establishes a security vulnerability or a false positive.
The separate RustSec audit remains unchanged and mandatory.

Use a reviewed, exact baseline ratchet, not package-name ignores, warning
suppression, continue-on-error, or unrelated dependency upgrades. Existing
findings remain unresolved debt in #108. Fail for newly reported identities,
changed versions/classifications or dependency requirements, malformed output,
scanner operational errors, and invalid baseline metadata. Do not compare
volatile age counters or scan timestamps. Preserve repository identity.

The reviewed source now replays into the combined CI-foundation worktree
`/home/leynos/Projects/VTCode.worktrees/hex-ci-prerequisites`, branch
`hex/ci-prerequisites`, based on dddd1352. Commit
`0b51c9ba1bf2b09b74f55201e06c882fb3589894` and its original branch remain
recovery and review provenance; do not edit that worktree. Other agents own
the ACP candidate and Stack #37.

## Ownership and tolerance

A journeyman owns this bounded prerequisite and delegates implementation to
artisans and reconnaissance to wyvern. All executable checks, tool invocations,
formatting, tests and review run only through the globally serial scrutineer.
Root granted the slot to this combined source after the ACP release candidate
completed its installed-path smoke.
Read AGENTS.md, python-router, execplans and relevant review/stack skills.
Context Pack is unavailable because of an unrelated oversized pack; exchange
exact paths and SHAs instead. Do not mutate that pack.

This reviewed ratchet owns six paths: this plan, a Python standard-library
checker, its behavioural tests, a JSON baseline, the existing CI workflow and
the existing dependency-maintenance guide. It is combined with the ten-path
CI-prerequisite component, sharing `.github/workflows/ci.yml`, for exactly
fifteen paths. No Cargo dependency or production Rust changes are added.
Keep the existing CI job identity, trigger, permissions and pinned scanner.
Use an argument-array subprocess, never shell interpolation. Never refresh
the baseline automatically in CI or accept arbitrary user-supplied baseline
paths from workflow event text. No credentials or paid probes are needed.

## Progress

- [x] Root created the clean branch and approved the bounded contract.
- [x] Inspect the pinned scanner JSON schema and design canonical identities.
- [x] Implement checker and adversarial behaviour tests through an artisan.
- [x] Capture actual JSON in the serial slot and classify exact baseline.
- [x] Run deterministic gates on the original reviewed component; final
  combined-source CodeRabbit CLI review remains pending.
- [x] Commit and hand root replayable provenance patch
  `0b51c9ba1bf2b09b74f55201e06c882fb3589894`.
- [x] Replay the exact patch into the combined fifteen-path CI-foundation
  source after `git apply --check` succeeded.
- [x] Run final deterministic gates on the combined source: thirteen checker
  tests and the live accepted 28-identity scan passed with the full native
  packet.
- [x] Run CodeRabbit on the exact staged documented source; it returned zero
  findings in `/tmp/coderabbit-VTCode-hex-ci-prerequisites-2.out`.
- [x] Publish the focused CI-foundation draft
  [#110](https://github.com/leynos/vtcode/pull/110) after commit
  `62548b081afb01014e787a812a504e4ab8a2eec6`.

## Work and validation

M1 is one complete milestone. Inspect cargo-unmaintained 1.10.0's installed
source, especially serialize.rs, repo_status.rs and exit handling in lib.rs.
Use its actual JSON field types. The human witness is
/tmp/acp-installed-release-unmaintained.out. The 27 names exist in the current
candidate and both 8e436821 and dddd1352 lockfiles. Confirm exact versions
and graph identities before adding baseline entries.

The checker invokes cargo unmaintained --json --no-warnings. Exit 0 means no
findings, 1 means findings, and 2 means an operational failure. Unknown exit
codes and contradictory exit/output combinations fail closed. Display every
accepted existing finding with its issue reference and category, and fail
on unrecognised findings. Missing old findings may pass but should be reported
as baseline entries eligible for manual removal. Baseline entries must include
exact semantic identity and an explicit classification/rationale linked to #108.
Do not label all repository-layout reports false positives: some are unverified.

Prefer one small canonicalisation function driven by the observed schema.
Tests must exercise actual wrapper behaviour with a fake scanner executable:
known findings pass visibly; new package, version, repository, status or stale
requirement fails; malformed/duplicate baseline or scanner JSON fails; exit2
fails even with plausible JSON; clean output passes; tool launch errors fail.
Argument handling must preserve literal paths and never execute shell syntax.
Fixtures should follow the pinned serializer, not an invented approximation.

Capture actual scanner JSON only after root grants the serial gate slot. Do
not rerun scans concurrently with candidate gates. The scanner uses shared
default Cargo and repository caches; no isolated cache or /tmp build target.
Use /tmp only for logs/scratch. Stop if disk fills; do not kill other jobs.

Validate Python syntax and behavioural tests, actionlint, changed Markdown,
diff checks and the actual pinned checker on this branch through scrutineer.
Because the checker and CI workflow are code changes, the standalone branch
must also pass the repository `make -j1 typecheck` and its complete Make
target equivalent (`make -j1 lint build test test-harness advisory`) before
CodeRabbit review. The only excluded target is optional ast-grep: its
executable is a known permission-denied path and must not be retried. Record
that unavailable target distinctly rather than calling literal `make check`
green; record any other inherited failures without silently repairing unrelated
source. The integrated release's full Rust gates remain mandatory as separate
evidence. Run `coderabbit review --agent` only after these deterministic gates
succeed.
On rate limit, use vsleep for a random 45-90 minutes, then retry. Resolve
applicable concerns, repeat affected checks, and commit conventionally.

## Integration and outcome

The combined CI-foundation draft is [#110](https://github.com/leynos/vtcode/pull/110),
from commit `62548b081afb01014e787a812a504e4ab8a2eec6`. Root owns stack
insertion, hosted CI, review coordination and merge. The candidate remains
blocked on this prerequisite until the hosted combined gate passes; never claim
the underlying maintenance debt has been fixed. The reviewed ratchet and
changed-Markdown workflow repair coexist in the shared CI file without new
behaviour beyond their individual patches.

The reviewed component has been replayed without conflict. Final combined
deterministic validation and CodeRabbit CLI review are green; hosted CI and
external review equilibrium remain pending. No scanner policy has been
weakened.

## Verification plan

The checker preserves five invariants. It treats the scanner process as an
untrusted boundary: a zero exit status proves an empty JSON list only, a one
exit status proves a nonempty JSON list only, and every other exit status or
launch failure is an error. It accepts only the exact public JSON shape emitted
by cargo-unmaintained 1.10.0. It compares a finding's name, version,
repository-status variant and complete sorted stale-requirement list, while
deliberately excluding the volatile `Age` value. It requires repository,
schema and issue metadata on every baseline entry. It reports missing baseline
entries without accepting new scanner identities.

The semantic identity lemma is that canonicalising the same supported scanner
finding and baseline entry yields the same value exactly when their stable
maintenance classification inputs agree. The implementation proves this over
the finite serializer variants with `unittest` fixtures, including `Age` and
`Unassociated`; the real serial scanner capture confirms that the fixtures are
not an invented wire shape. The subprocess lemma is exercised with a fake
executable whose literal path and arguments contain shell-looking characters.

The non-trivial external axiom is that cargo-unmaintained 1.10.0 retains the
serializer and documented process semantics inspected in its installed source:
its stdout JSON is an array, its exit statuses are 0, 1 or 2, and its JSON
status enum has the observed externally-tagged representation. Pinning the CI
installer to 1.10.0 makes that a bounded compatibility contract; a tool update
must fail closed and deliberately update both checker and baseline.

- **Obligation:** Scanner/baseline shape and identity
  - **Evidence:** Stdlib behavioural tests with fake scanner
  - **Discharge condition:** Known finding passes; malformed, duplicate, new or
    changed findings fail
- **Obligation:** Process-boundary failure handling
  - **Evidence:** Fake scanner exit and launch cases
  - **Discharge condition:** 0/1 contradictions, 2 and unknown exits, and launch
    errors fail
- **Obligation:** Stable-age treatment
  - **Evidence:** `Age` fixtures with different values
  - **Discharge condition:** Equal stable identity passes without comparing age
    count
- **Obligation:** Real scanner compatibility
  - **Evidence:** Serial `cargo unmaintained --json --no-warnings` capture
  - **Discharge condition:** Every observed identity classifies into tracked JSON
    baseline; entry count is recorded rather than assumed
- **Obligation:** CI wiring and documentation
  - **Evidence:** actionlint, changed-Markdown, diff checks
  - **Discharge condition:** Existing job identity/pins/permissions remain;
    workflow invokes fixed checker

## Surprises & discoveries

- The installed scanner source confirms an experimental JSON interface. It
  serializes `repo_status` as a unit string such as `Unassociated` or as an
  externally-tagged age object such as `{ "Age": 377 }`; the checker must
  reject any other representation.
- The earlier human-output summary reported 27 packages, but the raw
  `/tmp/acp-installed-release-unmaintained.out` and serial JSON capture show
  ten stale-requirement reports and eighteen repository-membership reports.
  The human format cannot provide the exact JSON versions and requirements
  required for the baseline. The serial capture is authoritative because a
  package name may have more than one locked semantic identity.
- The current workflow retains its `unmaintained` job, checkout hardening,
  pinned install action and scanner version. The approved change must alter
  only its final command.
- The serial capture ran `cargo unmaintained --json --no-warnings` through the
  Cargo subcommand, not the `cargo-unmaintained` wrapper directly. Version
  evidence is `cargo-unmaintained 1.10.0` in
  `/tmp/issue108-scanner-version-cargo.out`; the scan returned 1 with a valid
  28-entry JSON list at `/tmp/issue108-capture.stdout.json` (SHA-256
  `9edba544a677e87b941360e47009ecf57224216e0f4f298a5b988554a5056689`).
  Its separate stderr log is `/tmp/issue108-capture.stderr.log`.
- The additional semantic identity is `redox_users@0.5.2`. Every captured
  name-plus-version identity appears once in `Cargo.lock`; no package names or
  versions were silently dropped from the baseline.

## Decision log

- 2026-09-07: Keep the scanner's `Age` status kind but omit its numeric value
  from canonical identity because it is explicitly volatile; retain all other
  observed semantic fields.
- 2026-09-07: Treat unrecognised JSON fields and enum variants as a scanner
  compatibility failure. This makes a future tool-schema change visible before
  CI accepts it.
- 2026-09-07: Do not use Cargo metadata ignores or `--no-exit-code`; the
  checker, rather than the scanner, decides whether the exact debt baseline is
  accepted.
- 2026-09-07: Preserve the scanner's 28 captured semantic identities, not the
  prior human-witness count of package names. `redox_users@0.5.2` is a distinct
  stale-requirement finding and receives the same explicit #108 classification
  treatment as the other transitive age reports.
- 2026-09-07: The standalone branch passed `make -j1 typecheck` and the
  complete `lint build test test-harness advisory` target set. Ast-grep remains
  explicitly unavailable because its executable is permission denied; no retry
  or suppression was used.
- 2026-09-07: Root combined this reviewed six-path ratchet with the ten-path
  CI-prerequisite component because hosted CI needs both. The shared workflow
  produces a fifteen-path CI-foundation source. Original branch and commit
  remain provenance; final gates and CodeRabbit must run on the combined tree.

## Risks and hand-off

The global serial slot allowed the capture and deterministic gates to finish
without concurrent cache pressure. The artisan owned only the checker and its
tests; this lead integrated the actual JSON baseline, CI wiring and guide.
The remaining risk is hosted delivery: the local combined-source evidence does
not replace required hosted CI or external review. Root coordinates the stack
position and merge after those checks reach equilibrium. The coverage target and
LCOV witness remain untracked local artefacts because guarded cleanup was
rejected; neither is part of the committed source.

## Outcomes & retrospective

The ratchet now preserves 28 reviewed maintenance identities from the pinned
1.10.0 scanner rather than muting its findings. The captured baseline retains
the direct `better-panic` debt, transitive stale requirements, target-package
layout reports and repository-membership reports as separately visible #108
work. Behavioural tests cover the scanner process boundary, strict schema and
canonical identity. Historical component evidence comprises 13 checker tests,
the live 28-entry checker, actionlint, changed-document Markdownlint, diff
checks, `make -j1 typecheck`, and the complete Make target equivalent recorded
above. The first CodeRabbit review completed with zero findings before
standalone gates due an ordering correction. The final combined-source
deterministic packet is green; the final staged CodeRabbit review returned
zero findings before commit `62548b081afb01014e787a812a504e4ab8a2eec6` and
draft PR [#110](https://github.com/leynos/vtcode/pull/110).
