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
- [Draft PR #24](https://github.com/leynos/vtcode/pull/24) publishes
  `harden-lint-spelling-behaviour-apis` at
  `bb3ac5e61dcac47a6b05224f346a1c5e8f47c737`, based on #23.
  It carries model-behaviour configuration, tool-behaviour registration, and
  permission-decision types with all callers. Existing wire keys remain fixed.
  Its restacked formatting, lint, build, typecheck and full tests passed
  (10,082 passed, 17 skipped), along with all 67 harness tests.
- [Draft PR #25](https://github.com/leynos/vtcode/pull/25) publishes
  `harden-lint-spelling-behaviour` at
  `2874c2d106825d7b081c6c565dff7a44af58f06f`, based on #24.
  It covers the remaining native behaviour names and nearby prose, preserving
  the existing configuration wire property. The first full run passed formatting,
  lint, and build but exposed two dependent text mismatches. The prompt golden
  now follows the two changed source sentences; placeholder validation accepts
  both native and legacy spellings as unfinished template text. Full gates
  passed on the original retry: 10,079 tests, 17 skipped and 67 harness tests.
  Formatting, lint, build, advisory, VS Code bundling, and changed Python/shell
  syntax checks passed. Optional ast-grep remained unavailable.
- [Draft PR #26](https://github.com/leynos/vtcode/pull/26) publishes
  `harden-lint-spelling-colour-apis` at
  `33423a011ae5ae4768da06717b71b27ed829463a`, based on #25.
  It carries the reviewed native colour APIs with declarations and all
  consumers together, including CLI aliases, schema pins, and snapshot text.
  The layer slightly exceeds the soft 150-file target; a directory-based split
  was rejected because it separated APIs from their application callers.
  The first lint run exposed a missed native `flavour()` caller in the theme
  registry. It is repaired and the original full retry passed: 10,080 tests,
  17 skipped, and 67 harness tests. Formatting, lint, build, advisory, WebMCP
  typecheck, all 41 WebMCP tests, and production build passed. A dedicated CLI
  regression covers canonical and legacy flag parsing. Optional ast-grep was
  unavailable; the four inherited rustdoc warnings remain.
- [Draft PR #27](https://github.com/leynos/vtcode/pull/27) publishes
  `harden-lint-spelling-colour` at
  `431ab1bc4a173afb5dfa0208d41b33878f159174`, based on #26.
  It covers remaining native colour methods, table alignment, local bindings,
  terminal prose, and module guidance. External schemas and flags remain fixed.
  Its original Rust gates passed: 10,080 tests, 17 skips and 67 harness tests.
  Formatting, lint, build, advisory, Python compilation, and shell syntax
  passed. Supplementary WebMCP typecheck, 41 tests, and build also passed.
  The actual VS Code extension bundle passed. Optional ast-grep remained
  unavailable. Logs use the remaining-colour branch suffix. The two module
  guidance files retain 46 inherited Markdownlint findings, with no added or
  removed diagnostics; the handoff, Nixie, and cached diff checks passed.
- [Draft PR #28](https://github.com/leynos/vtcode/pull/28) publishes
  `harden-lint-spelling-catalogue` at
  `b06ef5e694d222c0ad273d681180b78ef3df75f6`, based on #27.
  It renames native catalogue APIs, modules, provider adapters, and callers,
  preserving serialized and persisted names. Formatting, lint, and build
  passed initially, but the full suite found a missing native skill-discovery
  keyword. Both `rule-catalogue` and legacy `rule-catalog` are now retained,
  with query regressions. The original full retry passed: 10,081 tests, 17 skipped,
  and 67 harness tests. Formatting, lint, build, advisory, and changed Python
  compilation passed. Optional ast-grep remained unavailable.
- [Draft PR #29](https://github.com/leynos/vtcode/pull/29) publishes
  `harden-lint-spelling-fulfilment` at
  `efb89ce02714fec24942146416d7becb0c56494a`, based on #28.
  It adopts native fulfilment, sceptic, and cancelled names with callers,
  preserving established wire values and adding an A2A serialization check.
  Its original full gates passed: formatting, lint, build, 10,081 workspace tests
  with 17 skipped, and all 67 harness tests. Changed shell syntax and advisory
  checks passed; optional ast-grep remained unavailable. Formatting and lint
  logs use the unnumbered fulfilment suffix; build, test, test-harness, shell,
  and advisory logs use `ACTION-2` with that suffix.
- [Draft PR #30](https://github.com/leynos/vtcode/pull/30) publishes
  `harden-lint-spelling-oxford` at
  `3a7c80a4821147d5d27fae46b8d7e392e8be70e1`, based on #29.
  It applies the reviewed Oxford source patch, including normalized code-search
  APIs and their callers. A seven-file residual patch updates authored source
  text and future plan-name vocabulary to `harbour`; existing paths remain
  readable. Parser compatibility still accepts both finalizing/finalising.
  Its original old-config gates passed: formatting, lint, build, 10,081 workspace
  tests with 17 skipped, and all 67 harness tests. Advisory checks passed;
  optional ast-grep remained unavailable. Logs use
  `/tmp/ACTION-vtcode-df12-onboarding-harden-lint-spelling-oxford.out`.
- [Draft PR #31](https://github.com/leynos/vtcode/pull/31) publishes
  `harden-lint-spelling-source` at
  `20e8f952d4341a2cf50df0e65c119ff521fef80c`, based on #30.
  It combines authored core/provider/source prose with the complete native
  `AnsiColourEnum` alias caller set: 149 reviewed files plus this record.
  External colour types, fixed wire names, and discovery aliases remain intact.
  Its original old-config gates passed: formatting, lint, build, 10,081 workspace
  tests with 17 skipped, and all 67 harness tests. Nixie passed all 21 changed
  Markdown files. Markdownlint retains 881 inherited findings across 20 files;
  exact path, line, and rule identities match the parent, with none added or
  removed. Advisory passed; optional ast-grep remained unavailable. Logs use
  `/tmp/ACTION-vtcode-df12-onboarding-harden-lint-spelling-source.out`.
- [Draft PR #32](https://github.com/leynos/vtcode/pull/32) publishes
  `harden-lint-spelling-foundation` at
  `90858bff6edbde66bea79a9c68a91b1a903707db`, based on #31.
  It corrects authored foundation-crate prose and preserves the newly renamed
  colour aliases, existing wire values, and generated model metadata.
  Its original old-config gates passed: formatting, lint, build, 10,081 workspace
  tests with 17 skipped, and all 67 harness tests. The initial runner report
  cited a nonexistent full-test log; that claim was rejected before commit,
  and the actual full suite subsequently passed in `test-2`. Preliminary
  Markdownlint retains 87 inherited diagnostics with exact path, line, and
  rule identities unchanged; Nixie passed all six changed Markdown files.
  Advisory passed; optional ast-grep remained unavailable. Logs use the
  foundation branch suffix, with `test-2` for the verified full-suite run.
- [Draft PR #33](https://github.com/leynos/vtcode/pull/33) publishes
  `harden-lint-spelling-engineering-docs` at
  `d94310f9a6be1cd8b5ba29cdb7c87e39f6368202`, based on #32.
  It corrects engineering, guide, and harness prose. The colour guide moves
  to `COLOUR_GUIDELINES.md` with both documentation-map links updated together.
  Preliminary Markdown checks found 41 new table-spacing and line-length
  diagnostics. Those were repaired, along with a stale native catalogue path.
  The original 3,579 diagnostics match parent path, mapped line, and rule
  identities, with none added or removed. Nixie passed all 81 changed files.
  All seven original ordered code gates recorded exit zero: formatting, lint, build,
  10,081 workspace tests with 17 skipped, 67 harness tests, the optional
  ast-grep skip, and advisory. Logs use `ACTION-manifest` with the engineering
  branch suffix.
- [Draft PR #34](https://github.com/leynos/vtcode/pull/34) publishes
  `harden-lint-spelling-user-docs` at
  `b1328dc3fedc00e01da8a8688abceb19a2a53f21`, based on #33.
  It corrects 94 user, reference, and provider documents, including native
  colour aliases in examples. Fixed URLs, APIs, configuration keys, and
  protocol names remain intact. All seven original code gates recorded exit
  zero: formatting, lint, build, 10,081 workspace tests with 17 skipped,
  67 harness tests, the optional ast-grep skip, and advisory. Logs use
  `ACTION-manifest` with the user-docs branch suffix. Nixie passed all
  95 changed documents. Original table and wrapping repairs left 5,069
  inherited Markdown diagnostics against 5,074 in the parent: no added
  path, mapped-line, and rule identities; five inherited instances removed.
  Two residual Oxford spellings found by reconnaissance are queued for the
  next source-fix layer before the final spelling scan and gate.
- [Draft PR #38](https://github.com/leynos/vtcode/pull/38) tracks
  `harden-lint-spelling-residual`; the rebased candidate targets
  [Draft PR #34](https://github.com/leynos/vtcode/pull/34) at
  `b1328dc3fedc00e01da8a8688abceb19a2a53f21`. The original residual validation
  evidence below remains historical against its previous parent. This candidate
  has completed fresh gates and supplement provenance checks, recorded below.
  A fresh scan using
  the reviewed external configuration found 1,086 candidates across 310 files.
  These include fixed external names and intentional malformed inputs, so
  classification precedes source fixes and narrow contract exemptions.
  The scan used typos 1.50.1 and exited 2; its log is
  `/tmp/spelling-measure-2-vtcode-df12-onboarding-harden-lint-spelling-residual.out`.
  All seven ordered workspace gates passed: formatting, lint, build,
  10,081 tests with 17 skipped, 67 harness tests, the optional ast-grep skip,
  and advisory. Logs use `ACTION-manifest` with this branch suffix.
  WebMCP typecheck, 41 tests, and build passed; VS Code bundled successfully;
  Python compilation and shell syntax checks passed. The excluded Zed crate
  passed its WASM check. Exact-parent comparisons found no added supplementary
  failures: Zed retains 83 formatting hunks and three cache Arc-count test
  failures (149 tests pass); VS Code retains 578 TypeScript diagnostics.
  Markdown retains 3,384 inherited findings against 3,390 in the parent,
  with no added mapped identities; Nixie passed all 57 changed documents.
  A refined offline spelling scan found 335 candidates across 115 files;
  further native sample and command-skill corrections precede the gate.
- [Draft PR #51](https://github.com/leynos/vtcode/pull/51) tracks
  `harden-lint-spelling-examples`; the rebased candidate targets
  [PR #38](https://github.com/leynos/vtcode/pull/38) at
  `03ab606cc8ed15f29a5eb84e360fd872edd465ab`. The original examples validation
  evidence below remains historical against its previous parent. Fresh gates
  and supplementary comparisons are recorded below. The additional workflow
  validation remains a later onboarding layer. The command-skill
  migration retains the legacy ID, discovery precedence, and model-catalogue
  exclusion. Authored test prompts and examples follow in the same layer;
  all seven ordered workspace gates passed. The full run passed 10,090 tests
  with 17 skipped and one leaky-process result. Its focused repeat passed
  without a leak; the unchanged snapshot test has no process-spawn path.
  The 67 harness tests passed. Logs use `ACTION-manifest-2` with this branch
  suffix. Nixie, Python compilation, and VS Code bundling passed. The focused
  mention-parser suite passed 16 tests and reproduced the exact parent's one
  email-detection failure, tracked as
  [issue #48](https://github.com/leynos/vtcode/issues/48). Markdownlint retains
  the exact parent's 31 findings with no added or removed identities.
  The proposed spelling scan now reports 137 findings
  across 58 files; the policy remains outside the live tree.
- The user now prioritizes review and merge readiness for #20 and subsequent
  PRs in order, using the comenq-coderabbit workflow and worker agents.
  GitHub cancelled #20's cold `tool-eval` job at its 15-minute ceiling after
  a 10m43s build; a 30-minute budget patch is prepared for that PR. Its first
  CodeRabbit review has six inline findings. Pertinent fixes are being
  prepared against #20; spelling and structural findings are assigned to the
  later train layers. Every bot reply must mention `@coderabbitai`. Valid
  out-of-scope findings require GitHub issues. Preserve the current layer as
  a gated commit before switching to the review fixes.
- Remaining spelling changes are preserved separately while each layer is
  validated. Later layers cover other native spelling groups, ordinary prose,
  and finally the spelling gate. Structural moves, source lint fixes, nightly
  formatting, the strict lint configuration, and Whitaker wiring follow.
- The local ACP/provider capability-hardening train remains separate and
  untouched. It will eventually rebase onto the lint train.
- The user waived all Lody-session requirements. Upper PRs remain draft and
  stacked, with predecessor/successor links, reviewer entrypoints and gate evidence.
  The user marked #20 ready for review and applied a GitHub stack to the
  published train. Native stack #37 now contains 15 PRs, including #38;
  preserve it and the existing review states when adding layers.

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
The artefact and behaviour-API layers have now passed their own full gates.
The next remaining-behaviour patch was regenerated against the published API
commit to omit the caller updates that moved earlier. Its reviewed path is
`/home/leynos/Projects/vtcode-behaviour-remaining-v2-44i079/behaviour-remaining-reviewed-v2.patch`.
It passed isolated apply and byte/mode reconstruction. The full suite then
exposed two text dependencies, repaired in the live layer before the green
gate retry. Logs use
`/tmp/ACTION-2-vtcode-df12-onboarding-harden-lint-spelling-behaviour.out`.
Apply them only to their intended parent in
order, preserving the latest handoff and discovery compatibility repair.

The corrected full colour patch is
`/home/leynos/Projects/vtcode-colour-api-review-api-parent-h2R1q4/colour-api-reviewed.patch`
(SHA-256 `882e3b6d2a50484435d6d5feacc861c978b95c5fed5a5bf9ccfb7a73fcf90c1a`).
It passed static apply and byte/mode checks against the published behaviour
layer. Superseded crate/application split patches must not be used. The native
CLI option is `--no-colour`, with `--no-color` retained as an alias; fixed wire
keys and foreign types retain their established spelling. Rust retry logs use
`/tmp/ACTION-2-vtcode-df12-onboarding-harden-lint-spelling-colour-apis.out`;
WebMCP logs use `webmcp-ACTION` with the same branch suffix. The two changed
legacy colour documents have 27 inherited Markdownlint findings; an added
line-length finding was wrapped. The handoff, Nixie, and diff checks passed.

An offline documentation worker accidentally wrote 94 unstaged documentation
paths during the remaining-colour final checks. Those edits were preserved
with verified bytes and modes at
`/home/leynos/Projects/vtcode-prose-live-recovery-wq7ass3l`, then only those
paths were restored to the index. The staged source layer was unchanged.
The recovered edits remain available for the later user-documentation batch;
do not discard that recovery archive or restore it wholesale over later work.

The reviewed catalogue v2 patch is
`/home/leynos/Projects/vtcode-catalogue-review-v2-live-20260905.patch`
(SHA-256 `8f51e048ef723698217521d2f937f56ea28bac88da6c25741a405055910ff68c`).
It was rechecked against the published remaining-colour source and applied
with all current spelling repairs preserved. Its offline review corrected
missing imports and callers and preserved the OpenCode caching/vision logic.
The subsequent live discovery repair adds native and legacy catalogue keywords
and query regressions; preserve it in all later layers. Retry logs use
`/tmp/ACTION-2-vtcode-df12-onboarding-harden-lint-spelling-catalogue.out`.

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

## PR 25 review restack

The remaining behaviour spelling layer is rebased onto PR #24 `bb3ac5e61`.
Only this handoff conflicted. The registry test rename auto-merged while
preserving its body and the lower layer's fallible rstest fixture. The fixed
UI wire key, paired prompt goldens and both placeholder spellings remain
intact. All eight sequential gates passed, including typecheck, 10,083
workspace tests, 17 skips and 67 harness tests. Logs use `restack-1` on
`harden-lint-spelling-behaviour`; final handoff checks use `handoff-restack-1`.
The push uses an explicit lease against the original published PR #25 head.

## PR 26 review restack

The colour API layer is rebased onto PR #25 `2874c2d10`. The TTY module and
this handoff conflicted. Resolution preserves every lower-layer capability
case and assertion while applying the native colour names. Canonical and
legacy CLI flags, serde wire keys and aliases, and native flavour naming
remain intact. All eight sequential gates passed, including typecheck, 10,084
workspace tests, 17 skips and 67 harness tests. Logs use `restack-1` on
`harden-lint-spelling-colour-apis`; final handoff checks use `handoff-restack-1`.
The push uses an explicit lease against the original published PR #26 head.

## PR 27 review restack

The residual colour layer is rebased onto PR #26 `33423a011`. The replay
completed without conflicts and its non-handoff patch is byte-identical.
The residual APIs and the 29-line UI module guide remain intact. All eight
sequential gates passed, including typecheck, 10,084 workspace tests, 17 skips
and 67 harness tests. Logs use `restack-1` on `harden-lint-spelling-colour`;
final handoff checks use `handoff-restack-1`. The optional ast-grep scan was
skipped because the tool is unavailable. The push uses an explicit lease
against the original published PR #27 head.

## PR 28 review restack

The catalogue layer is rebased onto PR #27 `431ab1bc4`. The replay completed
without conflicts and the source patch is unchanged. Embedded catalogue
bytes, persisted paths, telemetry wire names, native and legacy discovery
queries, and separate vision and caching capabilities remain intact. All
eight sequential gates passed, including typecheck, 10,085 workspace tests,
17 skips and 67 harness tests. Logs use `restack-1` on
`harden-lint-spelling-catalogue`; final handoff checks use `handoff-restack-1`.
The optional ast-grep scan was skipped because the tool is unavailable. The
push uses an explicit lease against the original published PR #28 head.

## PR 29 review restack

The fulfilment, sceptic and cancelled spelling layer is rebased onto PR #28
`b06ef5e69`. The replay completed without conflicts; all 57 non-handoff paths
are byte-identical to the original layer. Fixed configuration keys, event
values and A2A cancellation protocol names remain intact. All eight
sequential gates passed, including typecheck, 10,085 workspace tests, 17 skips
and 67 harness tests. Logs use `restack-1` on `harden-lint-spelling-fulfilment`;
final handoff checks use `handoff-restack-1`. The optional ast-grep scan was
skipped because the tool is unavailable. The push uses an explicit lease
against the original published PR #29 head.

## PR 30 review restack

The Oxford source layer is rebased onto PR #29 `efb89ce02`. The replay
completed without conflicts. The cache replay helper and both property tests
remain intact alongside the normalized identity names; the hermetic registry
fixture and its six consumers are preserved. Generated `harbour` names,
existing supplied plan names and both parser spellings remain supported.
All eight sequential gates passed, including typecheck, 10,085 workspace
tests, 17 skips and 67 harness tests. Logs use `restack-1` on
`harden-lint-spelling-oxford`; final handoff checks use `handoff-restack-1`.
The optional ast-grep scan was skipped because the tool is unavailable. The
push uses an explicit lease against the original published PR #30 head.

## PR 31 review restack

The source-prose layer is rebased onto PR #30 `3a7c80a48`. The replay
completed without conflicts and its patch is unchanged. Lower cache tests,
TTY capability checks and startup environment locking remain intact. The
20 Markdown files outside this handoff and their parent versions are
byte-identical to the original comparison, preserving the inherited
881-diagnostic baseline. All eight sequential gates passed, including
typecheck, 10,085 workspace tests, 17 skips and 67 harness tests. Logs use
`restack-1` on `harden-lint-spelling-source`; final handoff checks use
`handoff-restack-1`. The optional ast-grep scan was skipped because the tool
is unavailable. The push uses an explicit lease against the original
published PR #31 head.

## PR 32 review restack

The foundation-prose layer is rebased onto PR #31 `20e8f952d`. The replay
completed without conflicts; all 80 non-handoff paths are byte-identical to
the original layer. The five crate guides and their comparison base blobs
are unchanged, preserving the 87 inherited diagnostic identities. All eight
sequential gates passed, including typecheck, 10,085 workspace tests, 17 skips
and 67 harness tests. Logs use `restack-1` on
`harden-lint-spelling-foundation`; final handoff checks use `handoff-restack-1`.
The optional ast-grep scan was skipped because the tool is unavailable. The
push uses an explicit lease against the original published PR #32 head.

## PR 33 review restack

The engineering-documentation layer is rebased onto PR #32 `90858bff6`. The
replay completed without conflicts. The testing and WebMCP guides retain the
lower hermetic-fixture and fail-closed sandbox requirements alongside their
spelling changes; the renamed colour guide and both map links are intact.
All eight sequential code gates passed, including 10,085 workspace tests
with 17 skipped and 67 harness tests. The optional ast-grep scan was skipped
because the tool is unavailable. Logs use `ACTION-restack-1` with the
engineering branch suffix.

The fresh Markdown comparison found 3,103 diagnostics in both the current
layer and its final parent, with no added or removed path, mapped-line, rule,
and multiplicity identities. The comparison handles the renamed colour guide,
spelling replacements, and unchanged end-of-file blank lines. Both raw logs
confirm that 81 files were linted. Nixie validated the 81 changed files.

The initial zero-diagnostic report was rejected because its logs contained no
linter output: `--format json` selected stdin mode rather than JSON output.
Corrected captures use `markdownlint-current-restack-2` and
`markdownlint-parent-restack-3` with the engineering branch suffix; the
comparison is `/tmp/vtcode-pr33-md-identity-root.json`. These fresh results
supersede the original diagnostic count for this restack.

## PR 34 review restack

The user-documentation layer is rebased onto PR #33 `d94310f9a`. Its 95-path
patch replayed without conflicts. Lower hermetic-fixture guidance, Anthropic
prefill and system-fallback documentation, and the hook experiment record are
preserved. Formatting, lint, build, type checking, all 10,085 workspace tests
(17 skipped), all 67 selected harness tests, and advisory checks passed. The
optional ast-grep scan remains unavailable. Logs use `ACTION-pr34-restack-1`
with the user-documentation branch suffix.

Fresh Markdownlint checked all 95 changed documents: 3,814 current diagnostics
against 3,819 in the final parent. Source-line mapping found no added identities
and five removed instances: one line-length finding in the LM Studio guide and
four table-spacing findings in the ANSI reference and styling architecture.
These are inherited failures, not a passing Markdownlint gate. Nixie passed all
95 files. Raw logs use `markdownlint-pr34-{current,parent}-restack-1`; the mapped
comparison is `/tmp/vtcode-pr34-md-identity-root.json`. This evidence supersedes
the original diagnostic counts for this restack.

## Additional Markdown-formatting requirement

The onboarding now also includes a tracked-Markdown format gate imported from
Netsuke revision `45174acf560d7810f87eb584c535986abbce5216`. Its checker
formats temporary copies in one batch, preserves tracked files, accepts exact
LF or CRLF canonical output, and fails closed on discovery or tool errors.
Process-boundary tests cover unusual filenames, deleted and untracked files,
empty discovery, mixed line endings, and the large-file broken-pipe regression.

Mechanical formatting of the complete tracked Markdown baseline precedes the
configuration that requires it. CI will install prebuilt mdtablefix 0.5.1 using
shared-actions revision `c5a54701c8603a0fa756a6b34c49bc2af75a6c11`; every
shared action and reusable workflow reference must use that revision. The main
Rust toolchain remains unchanged. Checker, CI-contract tests, developer docs,
and baseline formatting are being prepared separately; none is delivered yet.

Validation for that addition runs formatter application, focused checker and
workflow tests, then `check-fmt`, `test`, `typecheck`, `lint`, `markdownlint`,
and `nixie` sequentially. The PR #20 review loop remains a separate task under
its own configuration. Distinct worktrees use separate Cargo target directories
while retaining the shared default Cargo package cache.

## PR 38 review restack

The residual layer is rebased onto PR #34 `b1328dc3f`. All eight sequential
code gates passed: formatting, lint, build, type checking, 10,085 workspace
tests (17 skipped), 67 harness tests, the optional ast-grep skip, and advisory.
The optional scan is unavailable, rather than a completed scan. Gate logs use
`ACTION-pr38-restack-1` with the residual branch suffix.

Markdownlint checked 57 changed documents: 3,384 current diagnostics versus
3,390 in the final parent. Source-line mapping found no added identities and
six removed instances. The rewritten parser-guide subtitle retains its existing
MD036 finding; its corresponding source line was checked explicitly. Three
obsolete list entries and three unsupported configuration rows account for the
removed findings. These inherited failures are not a passing Markdownlint
gate. Nixie passed all 57 files. The mapped comparison is
`/tmp/vtcode-pr38-md-identity-root.json`.

The original Zed, VS Code, and WebMCP supplement results above are carried
forward, not rerun. Exact blob and mode comparison found identical 16-path
changes across the original and rebased transitions. The four inherited
WebMCP and Cargo differences match the parent transition. Evidence is in
`/tmp/pr38-supplement-parity-final.json`; inherited supplement failures remain
reported separately from the passing workspace gates.

## Additional GitHub Actions validation requirement

Import Netsuke's yamllint and actionlint validation, including hardening from
revision `d0bb051fac02416cb38a8e76865b1b569d5ec163`. Preserve the existing
workflow-security checks, require yamllint before actionlint, and test failure
propagation. Pin yamllint 1.38.0 and actionlint 1.7.12; verify the actionlint
archive before installation and use the reviewed installer revision.

Workflow repairs precede the gate that requires them. The offline candidate
and process-contract tests are under review and are not delivered. This
addition retains mdtablefix 0.5.1, the requested shared-actions revision, and
the project's unchanged primary Rust toolchain.

## PR 51 review restack

The authored-examples layer is rebased onto PR #38 `03ab606cc`. Its 30-path
inventory is unchanged. The Anthropic test input retains lower-layer test
repairs, and the handoff preserves both layers' history and new requirements.
All eight sequential code gates passed: formatting, lint, build, type checking,
10,094 workspace tests (17 skipped), 67 harness tests, the optional ast-grep
skip, and advisory. Logs use `ACTION-pr51-restack-1` with the examples branch
suffix. The optional scan remains unavailable.

Fresh supplementary checks passed Nixie for three documents, Python
compilation, and VS Code bundling. The mention-parser suite passed 16 tests
and reproduced the exact parent's email-detection failure from issue #48.
Markdownlint initially found one new handoff line-length finding; it was
wrapped before the final document checks. The other 31 diagnostic identities
belong to the native-plugin guide and match the final parent. The renamed
bundled command-skill document is clean. These inherited supplementary
failures remain visible and do not represent passing gates.

PR #20 has separately published review fixes at `8f34d6e94`. Its hosted eval
checks passed, and its next CodeRabbit review is queued. The upper train must
be restacked in order after that review reaches equilibrium and PR #20 merges.
The Markdown and workflow-validation candidates remain separate offline work;
none of their demanding configuration is enabled by this examples layer.

## Final native spelling source layer

`harden-lint-spelling-final-sources` follows PR #51
`450782900cd8dc542d6d1bfa5bf488c4925a7d24` and applies the reviewed nine
native-spelling corrections across seven paths. The changes cover authored
prose, test prompts, comments, and command examples; the canonical command is
`analyse`. Reviewed external names and syntax remain deliberate, including the
`rust-analyzer` project name and registry configuration shape. No spelling
configuration, exception, or policy change is introduced by this source layer.

The Zed `allowed_tools` example now uses the canonical command spelling, but
whether that configuration is enforced is a separate contract issue tracked in
[issue #65](https://github.com/leynos/vtcode/issues/65). This layer does not
claim to implement or validate that policy.

Fresh format, lint, build, type-check, workspace, and harness gates passed:
10,094 workspace tests passed with 17 skipped, and all 67 harness tests passed.
The advisory target passed in warning mode; optional ast-grep was unavailable.
Documentation generation retains the four previously recorded warnings.
Python compilation and Nixie passed. The Markdown and workflow-validation
prerequisites remain separate until their own full gates pass.

The subsequent tracked-index measurement found eight correctable textual hits.
This layer corrects the native `normalize` assertion prose and Copilot guidance
prose. The two stale guidance references had no CodeGraph symbol or code-pattern
wiring, so they were removed rather than renaming or inventing a tool API. The
actual CLI keeps canonical `analyse` and its legacy alias.
The earlier permission-hook JSON key remains accurate and
unchanged; its precise external-contract policy treatment is separate.

The proposed tracked-file spelling scan checked 3,095 existing paths and found
zero textual findings, plus 22 informational binary-file records. Its inherited
CHANGELOG exclusion is being replaced separately: the history audit identified
196 prose corrections, 137 commit-hash fragments, and 22 historical identifiers
or paths. The fixed references must remain byte-exact.

The first Markdown check found 334 diagnostics against 333 on the exact parent.
The sole added diagnostic was the rewritten Copilot instruction's line length;
that instruction is now wrapped. Final Markdown and spelling checks include this
handoff update. The inherited Markdown findings remain for the formatting work.

## Workflow source prerequisite

The workflow repair layer follows source spelling PR #66 and prepares the
existing five workflows for Netsuke's yamllint and actionlint policy. It adds
document markers, corrects comment spacing, expands a compact mapping, and
wraps long commands without changing their arguments or folded Rust flags.
Tag polling still makes 12 attempts with ten-second sleeps; diagnostics now
include the attempt number. The binary-size fallback keeps its existing
platform-specific probes and unknown-size outcome.

The source revision is `d0bb051fac02416cb38a8e76865b1b569d5ec163` from Netsuke.
Parsed YAML comparison preserves the CI, coverage, tool-eval, and WebMCP jobs
exactly. Build-workflow differences are confined to the documented equivalent
command forms and retry diagnostics. The Rust toolchain file remains byte-exact.
This layer carries source repairs only; the next layer installs the linters,
tests the process boundaries, and enables the gate.

This layer passed format, lint, build, type-check, workspace, and harness gates:
10,094 workspace tests passed with 17 skipped, and all 67 harness tests passed.
Advisory checks passed in warning mode. Optional ast-grep was unavailable, and
the same four rustdoc warnings remain under the existing configuration. Final
document and proposed workflow-linter checks include this evidence update.

## GitHub Actions validation gate

The validation layer follows workflow repairs PR #67 and imports Netsuke
revision `d0bb051fac02416cb38a8e76865b1b569d5ec163`. `make lint` now runs
yamllint before actionlint, alongside the existing security checks. Either
linter's non-zero status stops the gate. The policy requires YAML document
markers and 120-column lines while preserving GitHub's `on` trigger key.

CI pins yamllint 1.38.0 and actionlint 1.7.12. It verifies the actionlint
archive checksum before invoking the installer at its reviewed commit. The
exported downloader function and its archive variables cross the child-process
boundary together, so installation consumes the verified bytes. Controlled
tests cover every linter exit status and fail-closed download/checksum paths.
CI uses the absolute Make binary, preserves its four-job build limit, and
triggers on Makefile changes as well.

The primary Rust toolchain, existing workflow security policy, and job selection
are preserved. Markdown formatter installation remains a separate layer.

This layer passed all 13 contract tests, format, lint, build, type-check,
workspace, and harness gates: 10,094 workspace tests passed with 17 skipped,
and all 67 harness tests passed. Advisory checks passed in warning mode;
optional ast-grep was unavailable. The same four rustdoc warnings remain.
Final contracts, workflow linters, and document checks include the CI job-limit
correction and this evidence update.

## Historical CHANGELOG spelling layer

This layer applies 196 reviewed prose-only spelling corrections to `CHANGELOG.md`.
It preserves 137 commit-hash fragments and 22 historical identifiers or paths;
18 newly introduced single trailing spaces on changed release bullets were removed.
No generated spelling configuration is included. Format, lint, build, type-check,
workspace, and harness gates passed: 10,094 tests passed with 17 skipped, plus
all 67 harness tests. The 13 workflow contracts passed. Advisory checks passed
in warning mode; optional ast-grep was unavailable. Four inherited rustdoc
warnings remain.

The replacement history policy checked all 3,099 tracked files with no textual
findings. Markdownlint findings fell from 11,252 on the exact parent to 11,229,
with no additions; the remaining findings await the Markdown cleanup layers.
Nixie passed for both changed documents.

## Tracked-file spelling gate

This layer enables the reviewed Oxford spelling policy through `make spelling`
and `make lint`. Discovery uses Git's tracked-file index, preserves NUL-delimited
paths, skips deleted files, and fails closed on discovery errors. CI installs
pinned typos-cli 1.50.1 and invokes the same Make target.

Historical exceptions enumerate the audited commit hashes and names instead of
excluding changelogs. Custom file types match basenames, so both changelogs
remain covered. Focused process and CI contracts accompany developer setup
instructions. The primary Rust toolchain remains unchanged.

All eight spelling contracts, the 3,104-file spelling scan, and 13 workflow
contracts passed. Format, lint, build, type-check, workspace tests, and harness
checks passed: 10,094 tests passed with 17 skipped, plus all 67 harness tests.
Advisory checks passed in warning mode; optional ast-grep was unavailable.
Four inherited rustdoc warnings remain. Nixie passed both changed documents;
Markdownlint retains seven exact-parent line-length findings in the setup guide,
with no additions. CI installs typos in both spelling-dependent jobs.

## Module structure layer

This layer moves self-named Rust modules into `mod.rs` form and extracts API-key
and Anthropic validation sections without changing their behaviour. Forty-five
moves preserve their complete source bytes. The API-key production section and
all extracted test bodies retain their contents; Anthropic tool validation gains
only the visibility needed by its new private child module.

Ten relative module paths are adjusted for their new directories. Current
source-path guidance receives 35 literal substitutions across 23 documents;
dated historical records retain their original references. A full CodeGraph
refresh removed stale deleted-file entries, and Leta resolves the new API-key
module location. Existing logging and advisory rules follow their moved source
files with unchanged scope and rationale; model-script instructions use the new
path. One extracted test line is joined as required by the existing formatter.
The first full test run found 15 transcript-link failures caused by a fixture
path that still named the moved `session.rs`; the helper now follows
`session/mod.rs`. The corrected layer passes formatting, spelling, Actions
contracts, lint, build, type-checking, all 10,094 workspace tests (17 skipped)
and all 67 harness tests. Advisory checks pass; AST-grep is unavailable and the
existing optional target skips it. Four inherited rustdoc warnings remain for
the later source-fix layer.

All 24 changed documents pass Nixie. Exact-parent Markdown comparison identified
one new long line from a longer module path, now wrapped; the other 1,134
diagnostics are inherited. Final document validation follows that correction.

## Test-module extraction follow-up

This layer (`harden-lint-test-modules`) follows module structure PR #71 at
`630f2c7589d5860a1c8eca3772968903ba571566`. It extracts two source groups,
totalling 147 tests plus helpers: 105 OpenAI provider tests across 13 child
modules and 42 session tool catalogue tests across seven child modules.
Each extracted test module remains below 400 lines.

Test bodies are preserved. Private helper visibility and import wiring change
only as required by the new boundaries. Leading attributes and documentation
comments follow their corresponding items; the formatting preflight caught and
prompted correction of stranded item prefixes. Two function-local backend
imports also account for the added module depth.

The corrected layer passes formatting, lint, build and type-checking. All
10,094 workspace tests pass (17 skipped), preserving the original discovery
count; all 67 harness tests pass. The eight spelling and 13 Actions contracts
pass. Advisory checks pass; the existing AST-grep target skips because the
binary is unavailable. Four inherited rustdoc warnings remain for later
source fixes. The handoff passes Nixie; final Markdown and spelling checks
validate these notes.
The tool-output test extraction remains a subsequent layer.

## Changelog Markdown prerequisite

This layer follows test-module PR #72. It gives changelog sections unique
version-qualified headings while preserving non-heading history. The generated
release sections follow the same heading contract. Hermetic fixtures exercise
both release-script generators and pinned git-cliff without network access or
ambient Git configuration.

The fixture exposed a release-script defect: plain read loops skipped the final
Git log record when it lacked a trailing newline. Both loops now retain a
nonempty final record, preserving the oldest commit and contributor. Strict
category, hash and ordering assertions remain in place.

Offline checks passed the fixture and found no added Markdown diagnostics:
8,425 remain against 11,233 in the exact source parent, with all 2,808
differences being removals. Changelog duplicate-heading and multiple-title
findings are zero. ShellCheck retains 21 exact-parent diagnostics. Current
blobs match the audited candidate, and both parent document blobs match its
exact parent, so the diagnostic identity mapping still applies.

Current-layer formatting, lint, build, type-checking, all 10,094 workspace
tests (17 skipped), all 67 harness tests, eight spelling contracts and
13 Actions contracts pass. The generator fixture also passes on this branch.
Advisory checks and Nixie pass; optional AST-grep is unavailable. Four inherited
rustdoc warnings remain for later source fixes. Final handoff and spelling
checks validate these notes; mechanical Markdown formatting follows in later
layers.

## Agent test-module extraction

This layer (`harden-lint-agent-test-modules`) follows parent PR #73 at
`bfe2c66d`. It extracts four groups: 47 tool-output-handler tests,
60 tool-outcome helper tests, 69 system-prompt tests and 27 execution-history
tests, including two property-test blocks. Named, test-gated module roots
connect the child files. Shared helpers remain private to the test subtree.

Independent audits verify complete items, attached attributes and comments,
required import scope and authored diagnostic fixtures. In particular, the
synthetic `runner/tests.rs` strings remain unchanged. Child modules contain
at most 315 lines after formatting. Compilation exposed two move-sensitive
references: the system-prompt fixture include needs a parent-directory prefix,
and the extent-coverage child needs a distinct name to avoid shadowing the
production read-extent module. Both are corrected without changing fixture
bytes or assertions.

Formatting, lint, build, type-checking, all 10,094 workspace tests (17 skipped)
and all 67 harness tests pass. Eight spelling contracts and 13 Actions
contracts retain their passing results; their implementation is unchanged.
Advisory checks, handoff Markdownlint and Nixie pass. Optional AST-grep is
unavailable, and four inherited rustdoc warnings remain for later source fixes.
