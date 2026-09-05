# Rust baseline delivery handoff

Checkpoint date: 2026-09-05. Codex has restarted and delivery is continuing.
The complete PR train is not yet delivered.

## Train and source state

- Worktree: `/home/leynos/Projects/VTCode.worktrees/vtcode-df12-onboarding`.
- The unchanged starting commit
  `07e22369438205d9d712c78892168eff867c053f` matched `upstream/main` when
  fetched at setup. It is published as `origin/vtcode-df12-onboarding`.
  The fork's older `origin/main` is not the train's base.
- [Ready PR #20](https://github.com/leynos/vtcode/pull/20) publishes
  `test-quality-sweep` against that base at
  `e939836042efa32a61ae961aebd68ab4cc9afd57`.
- [Draft PR #22](https://github.com/leynos/vtcode/pull/22) publishes
  `harden-lint-spelling` at `b82c2d07a64f14696b68cfe9c8df7fe81938883f`, based
  on `test-quality-sweep`. It isolates native `analyse` API and command names,
  corresponding callers, module paths, and compatibility assertions. Existing
  `analyze` command invocations and fixed serialized names remain supported.
- [Draft PR #23](https://github.com/leynos/vtcode/pull/23) publishes
  `harden-lint-spelling-artefact` at
  `ae7936fe4f28ef6096791e3d0244f2cfb089ddf0`, based on #22.
  It renames native artefact APIs and modules, pins existing serialized names,
  and keeps the dependent prompt golden text with its source wording change.
  Formatting, lint, build, typecheck and the full suite passed (10,082 tests,
  17 skipped), as did all 67 harness tests and the advisory checks.
- `harden-lint-spelling-behaviour-apis` is the active layer, based on #23.
  It carries model-behaviour configuration, tool-behaviour registration, and
  permission-decision types with all callers. Existing wire keys remain fixed.
  Its original formatting, lint, build, and full tests passed (10,078 passed,
  17 skipped), along with all 67 harness tests.
- Remaining spelling changes are preserved separately while this layer is
  validated. Later layers cover other native spelling groups, ordinary prose,
  and finally the spelling gate. Structural moves, source lint fixes, nightly
  formatting, the strict lint configuration, and Whitaker wiring follow.
- The local ACP/provider capability-hardening train remains separate and
  untouched. It will eventually rebase onto the lint train.
- The user waived all Lody-session requirements. Upper PRs remain draft and
  stacked, with predecessor/successor links, reviewer entrypoints and gate evidence.

## Validation evidence

The published test-quality layer passed formatting, lint, build, full tests,
harness checks, advisory checks, and changed-document checks. Its full test
run passed 10,075 tests, with 17 skipped. The separate harness gates passed
67 tests across three suites. Logs use
`/tmp/ACTION-vtcode-df12-onboarding-test-quality-sweep.out`.
Four pre-existing rustdoc warnings remained under the old configuration.
Optional ast-grep was unavailable and was skipped by the existing gate policy.

Before partitioning, the combined spelling candidate passed workspace Clippy
under the old configuration:
`/tmp/lint-clippy-4-vtcode-df12-onboarding-harden-lint-spelling.out`.
This does not establish that each intermediate layer passes. Every layer must
run its own full gates before it is committed and published.

Additional combined-candidate checks passed: WebMCP type checking, all 41 tests,
and its production build; VS Code bundling; and changed Python/shell syntax.
VS Code's standalone TypeScript check reports 578 inherited diagnostics, all
also present in an archived parent checkout after path/name normalization.
No diagnostics were added. That supplementary check remains red; it is not a
passing gate. Comparison logs are
`/tmp/baseline-vscode-tsc-vtcode-df12-onboarding-harden-lint-spelling.out` and
`/tmp/typecheck-vscode-vtcode-df12-onboarding-harden-lint-spelling.out`.

The isolated `analyse` layer passed its old-profile formatting, lint, build,
full nextest suite (10,078 passed, 17 skipped), and three harness suites
(67 passed). Advisory checks, VS Code bundling, and changed Python/shell syntax
also passed. Optional ast-grep was skipped as unavailable, and the four
inherited rustdoc warnings remain under the old policy. Evidence uses
`/tmp/analyse-layer-2-ACTION-vtcode-df12-onboarding-harden-lint-spelling.out`.
The isolated layer's TypeScript check has the same 578 diagnostics as the
archived parent, with none added or removed after file-name normalization.
The handoff's Markdownlint, both changed documents' Nixie validation, and the
staged diff check passed. The embedded ast-grep skill retains 417 inherited
Markdownlint diagnostics, matching the parent with none added or removed.
No new suppression or exclusion was introduced.

The behaviour-API layer's first Clippy run found four tool-registration
accessor calls that belonged with the renamed API rather than the next
behaviour layer. The calls and adjacent bindings were moved into this layer;
a residual accessor scan was clean, and lint, build, and full tests passed.
Logs use
`/tmp/ACTION-2-vtcode-df12-onboarding-harden-lint-spelling-behaviour-apis.out`.

The artefact layer's first Clippy run found one omitted guard access to
`task.artifacts` in the A2A CLI. It was corrected to `task.artefacts`; a
native-identifier audit found no further omitted callers. The subsequent
lint, build, and full tests passed. Logs use
`/tmp/ACTION-2-vtcode-df12-onboarding-harden-lint-spelling-artefact.out`.

The first test run exposed a spelling-layer boundary: the skill-discovery
query used `analyse`, but its bundled metadata advertised only `analyze`.
This layer now carries both keywords and tests both query spellings. The
complete reviewed patch including that repair is
`/home/leynos/Projects/vtcode-analyse-review-hbssv_tp/analyse-reviewed-with-discovery.patch`.

## Preserved remaining work

The combined candidate is preserved in Git stash
`eeeb89cf05c50814e5ba879daeca6f6490a68188` and a verified external backup:
`/home/leynos/Projects/vtcode-spelling-full-wd8e3zpr`.
The backup includes a tar archive, path/content/mode manifest, original index,
and staged/unstaged patches. Do not drop the stash or overwrite the backup
until all remaining changes have been accounted for in the train.

The reviewed first-layer patch is
`/home/leynos/Projects/vtcode-analyse-review-hbssv_tp/analyse-reviewed.patch`.
The temporary classifier in `/tmp/vtcode-spelling-partition.py` is advisory.
Its raw later patches require review: exact final reconstruction does not
prove that intermediate API declarations, callers, or test bodies are intact.
A manual audit removed unrelated provider edits and kept command-skill and
alias-test changes atomic before applying the first layer. Subsequent reviewed
patches are prepared outside the worktree: artefact v2 at
`/home/leynos/Projects/vtcode-artefact-reviewed-vgEh5E/artefact-reviewed-v2.patch`
and behaviour APIs at
`/home/leynos/Projects/vtcode-behaviour-review-eByK3M/behaviour-reviewed.patch`.
They include dependent callers and fixture text omitted by the classifier;
the artefact patch also preserves field-purpose docs before wire-name notes.
They have passed isolated apply and byte/mode reconstruction, but have not
run their own repository gates. Apply them only to their intended parent in
order, preserving the latest handoff and discovery compatibility repair.

## Measurement and continuation

Read `/home/leynos/docs/rust-baseline-remediation-plan.md` before continuing;
VTCode lessons have been appended. The pinned Rust 1.93.0 toolchain already
requires rust-analyzer. Leta and CodeGraph were set up and used; refresh their
indexes after switching between candidate layers.

The scratch strict-config measurement reported 844 unique Clippy diagnostics
across 58 visible files. Leading counts were `str_to_string` 187,
`indexing_slicing` 122, `use_self` 56, `must_use_candidate` 51, and
`missing_docs` 50. These are lower bounds because failing crates hide later
waves. The scratch configuration was fully restored. Details are in
`/tmp/vtcode-baseline-counts.json` and `/tmp/vtcode-baseline-measurement.md`.

Spelling needs Oxford overrides: `normalize` and `economize` are correct;
`analyse` retains `-yse`. The latest combined scan reported 1,219 candidates
across 307 files, including fixed external names and remaining prose, before
subsequent corrections. Its log is
`/tmp/spelling-4-vtcode-df12-onboarding-harden-lint-spelling.out`.

Run all gates sequentially through tee, using the shared Cargo cache and the
existing target directory. Never use `/tmp` as a build target. Nextest runs
ordinary tests; the later explicitly requested doctest gate is the Cargo-test
exception. Scrutineer runs full gates. Exchange team code through context pack
`pk_s2nzbpoo`; use leta, rust-router, and CodeGraph. Use the Whitaker skill for
its findings and the PR-creation skill for every draft PR. Global Git
attributes configure Weave, so inspect any future merge resolutions carefully.

The ast-grep executable previously returned `Permission denied`; it was not
retried. Resolve that access issue before using it for structural work. The
historical fixture repairs and their evidence remain in
[the debugging record](debugging/debugging-plan-2026-09-05.md).

## PR 20 review follow-up

The user marked [PR #20](https://github.com/leynos/vtcode/pull/20) ready for
review and requested the comenq-coderabbit workflow through merge, followed by
worker-led review of each subsequent PR in order. The published train now
contains 16 layers through [PR #51](https://github.com/leynos/vtcode/pull/51),
in native GitHub stack #37. Original branch heads are preserved locally in
`refs/backup/rust-baseline-pre-review-20260905/` before any rebases.

The cancelled `tool-eval` job exceeded its 15-minute ceiling after a 10m43s
cold workspace build. The workflow now permits 30 minutes for the build and
PTY test compilation. Shared environment guards cover the complete temporary
environment scopes in steering and startup tests. The six command-session
tests use a fallible async rstest fixture that owns the registry, configuration
guard, and workspace. TTY tests independently cover all required capabilities.
Only rstest 0.26.1 and its two new transitive packages were added to the lock.
All seven review-fix gates passed. The workspace suite passed 10,075 tests
with 17 skipped; the three harness suites passed 67 tests. Evidence is in
`/tmp/*-review-1-vtcode-df12-onboarding-test-quality-sweep.out`. Handoff
Markdownlint and Nixie also passed; optional ast-grep remained unavailable.

Every CodeRabbit reply includes `@coderabbitai`. The two requested spelling
corrections are already in PR #34; the validation-module split is assigned to
the later structural layer and tracked in
[issue #52](https://github.com/leynos/vtcode/issues/52). Reconcile pre-merge
checks after inline findings
are settled, raise valid out-of-scope findings as issues, and preserve each
layer's full gates when rebasing before a push with `--force-with-lease`.

The first review repair is committed and pushed as `24758f226`. CodeRabbit
confirmed the four inline fixes; all six inline threads are resolved. The next
review wave adds payload-level Anthropic prefill regressions, a hermetic test
through the public WebMCP check boundary, and generated code-search replay
properties. Existing developer and provider guides document the gate, fixtures,
and prefill migration. All seven second-wave gates passed: 10,079 workspace
tests, 17 skips, and 67 harness tests. Logs use the `premerge-1` action suffix.
Changed-document Markdownlint is compared with the exact `24758f226` parent
baseline; diagram validation covers all five changed Markdown files.

Both CI runs on `24758f226` passed. The PR-triggered tool evaluation took
16m47s, providing direct evidence that the original 15-minute limit was too
short. Both hosted checks subsequently passed on `e084280e2`; its full review
is queued through comenq as `2d40c0d5`.

## Hook-chain fixture follow-up

The PR #23 restack gate exposed a scheduling-sensitive hook fixture inherited
from the initial base. A deterministic experiment reproduced its lost rewrite
when the first child exited before the parent wrote stdin. Draining stdin
before emitting the same JSON preserved the exact chain assertion. The test
now follows the existing hook guide and prints hook messages on assertion
failure. Production handling is unchanged and tracked in
[issue #54](https://github.com/leynos/vtcode/issues/54).

The experiment and its corrected process-ordering method are recorded in
[the debugging plan](debugging/debugging-plan-20260905-hook-stdin.md).
All seven PR #20 gates passed: 10,079 workspace tests, 17 skips and 67 harness
tests. Logs use the `hook-repair-1` action suffix. The optional ast-grep scan
remains unavailable. The first documentation check found one code-span spacing
error in the new debugging plan; it was corrected before final validation.
PR #23 remains unpublished at its restacked candidate. After this repair is
pushed, each upper layer must rebase onto its actual previous parent and pass
its own full gates before publication.

## PR 22 review restack

The analyse spelling layer is rebased onto PR #20 repair `e084280e2`. A plain
three-way merge preserved the upper-layer delivery record and appended the
PR #20 review evidence after it. The code patch matches the original PR #22
patch; the range-diff changes only the handoff context. All eight sequential
restack gates passed, including typecheck, 10,082 workspace tests, 17 skips,
and 67 harness tests. Logs use `restack-1` on `harden-lint-spelling`. Handoff
Markdownlint and Nixie also passed. The push uses `--force-with-lease`.
PR #20 review is queued through comenq; later PR reviews remain in train order.

The second restack carries PR #20 hook-fixture repair `e93983604`. Only this
handoff conflicted; the non-handoff patch identity is unchanged. The prior
restack results above describe the first published rebase. All eight gates
passed again, including typecheck, 10,082 workspace tests, 17 skips and 67
harness tests. Logs use `restack-2` on `harden-lint-spelling`; final handoff
checks use `handoff-restack-3`. Both hosted checks passed on PR #20's
`e93983604` head. Its full CodeRabbit review remains queued.

## PR 23 review restack

The artefact spelling layer is rebased onto PR #22 repair `b82c2d07a`.
The replay completed without conflicts and exactly matches the preceding
candidate's patch. The original candidate's full gate exposed the hook-fixture
race documented above; that repair now arrives through PR #20. Schema names,
compatibility aliases and both lower-layer review records remain intact.
All eight sequential gates passed, including typecheck, 10,082 workspace
tests, 17 skips and 67 harness tests. The hook-chain regression passed within
the full suite. Logs use `restack-2` on `harden-lint-spelling-artefact`;
final handoff checks use `handoff-restack-1`. The push uses an explicit lease
against the original published PR #23 head.

## PR 24 review restack

The behaviour API layer is rebased onto final PR #23 head `ae7936fe4`.
The replay completed without conflicts and its patch is unchanged. All four
accessor repairs and the permission-hook JSON key `behavior` remain intact.
All eight sequential gates passed, including typecheck, 10,082 workspace
tests, 17 skips and 67 harness tests. Logs use `restack-1` on
`harden-lint-spelling-behaviour-apis`; final handoff checks use
`handoff-restack-1`. The push uses an explicit lease against the original
published PR #24 head.
