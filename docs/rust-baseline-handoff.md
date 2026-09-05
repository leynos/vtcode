# Rust baseline restart handoff

Checkpoint date: 2026-09-05. The user requested a stop once the build is stable
to restart Codex with a larger agent-thread limit; that restart has now been
completed. The PR train is incomplete.

## Source and delivery state

- Worktree: `/home/leynos/Projects/VTCode.worktrees/vtcode-df12-onboarding`.
- Current branch: `test-quality-sweep`; HEAD is
  `07e22369438205d9d712c78892168eff867c053f`.
- A fresh fetch confirmed that HEAD equals `upstream/main`. The unchanged base
  is published as `origin/vtcode-df12-onboarding`, which is the intended base of
  the first draft PR. The fork's `origin/main` is an older ancestor; do not use
  it as the first PR base or rewrite it.
- No remediation commit or draft PR exists yet. Changes in the index and
  working tree are part of the first layer. Full layer gates have not passed.
- The separate local ACP/provider capability-hardening train stays untouched.
  It will eventually rebase onto the lint train, not the reverse.
- The user waived all Lody-session requirements. Every PR must still follow
  the `pr-creation` skill, be draft, use its predecessor as base, and link its
  neighbours with a reviewer walkthrough and validation evidence.

## Completed setup and measurement

Read `/home/leynos/docs/rust-baseline-remediation-plan.md` and repository
`AGENTS.md` before resuming. The recipe has additional VTCode lessons appended.
The pinned Rust 1.93.0 toolchain was installed; its existing component list
already includes rust-analyzer. Leta's workspace was registered. CodeGraph was
indexed and used for reconnaissance, although some individual symbol lookups
returned no matches and required live-source fallback.

Scratch branch `baseline-measurement` temporarily enabled the complete proposed
lint configuration and inheritance for all 22 workspace members. That scratch
configuration was restored before creating this branch. The current layer uses
the old lint and formatting configuration, as required by the train order.

Clippy emitted 844 unique diagnostics across 58 visible files. Leading lints
were `str_to_string` (187), `indexing_slicing` (122), `use_self` (56),
`must_use_candidate` (51), and `missing_docs` (50). These are lower bounds:
failed dependency crates hide later waves. Rustdoc also failed in early crates.
Use `/tmp/vtcode-baseline-counts.json`, `/tmp/vtcode-baseline-measurement.md`,
and the scratch measurement logs for detailed counts. Static file sizes show
that later structural and lint-fix layers will need several area PRs.

## Current first-layer changes

- A new sequential Makefile mirrors existing release checks and explicitly
  covers the whole workspace and all features in Clippy and nextest. Existing
  scripts omitted workspace coverage in their nextest path. `.gitignore` now
  permits the root Makefile.
- Event serialization tests share a generic match-panic round-trip assertion;
  decorative Result plumbing was removed from those tests and four in-memory
  tokenizer tests. Real I/O failures remain fallible.
- The root configuration integration test injects temporary workspace paths
  and empty home/system search lists, avoiding ambient configuration reads.
- Watcher, memory-cache, and template-cache fixtures set distinct modification
  times explicitly. `filetime` was already locked and is now a direct test
  dependency of the relevant crates.
- Provider command-environment tests inject synthetic snapshots through a
  private extraction; production still supplies the inherited environment.
- Parent-death signal tests use isolated rusty-fork children with bounded waits
  and success/SIGTERM assertions. The old test passed its own PID as the parent
  and terminated the test harness. The dependency was already in Cargo.lock.
- The project-memory path test uses its existing directory override and an
  exact workspace-owned path assertion.
- PTY setup is fallible and supplies an isolated tool policy plus anchored
  fixture command rules. A test-only defaults guard under `tests/support/`
  survives asynchronous construction and restores the previous provider.
- Several stale model fixtures now name explicit wire IDs or current registry
  entries. The OpenCode Zen positive catalogue assertion was preserved after
  review; converting it to an unknown-model assertion would lose coverage.
- One effort-description literal again names both Opus 4.8 and 5, matching
  existing presets and the existing assertion. No capability logic changed.
- The generic raw-extent check for `code_search` now excludes `CODE_SEARCH`,
  whose normalized identity already includes its limit and filters. Existing
  positive coverage and the different-limit negative case remain.

## Validation and remaining work

The pre-resume full suite ran 10,084 tests: 10,025 passed, 59 failed, 17
skipped. Its log is `/tmp/test-2-vtcode-df12-onboarding-test-quality-sweep.out`.
That 59-failure total is historical and predates the current fixture repairs.
After the restart, `make test` completed 10,085 tests: 10,059 passed, 26
failed, and 17 skipped. Its log is
`/tmp/resume-test-vtcode-df12-onboarding-test-quality-sweep.out`. All PTY
tests, H7 parent-death tests, and H8 project-memory tests passed in that run.
The first 50 focused event/tokenizer/config tests also passed. A prior selected
run passed 21 of 22 tests; Gemini thought-signature preservation still failed
after fixing an invalid positional lookup.

The latest completed full suite ran 10,075 tests: 10,075 passed and 17
skipped. Its log is
`/tmp/test-vtcode-df12-onboarding-test-quality-sweep.out`. The earlier ten
failure groups are cleared: seven prompt-wording or golden cases, two startup
duplicate-TOML-table cases during raw serialization and append, and one Gemini
legacy-reasoning case. The run also passed the two new WebMCP tests, the
prequeued-stop, compaction, pipe, and PTY cases.

Before the restart, the final checkpoint passed `make check-fmt`, `make lint`,
and `make build`. Logs use
`/tmp/checkpoint-2-ACTION-vtcode-df12-onboarding-test-quality-sweep.out`, where
ACTION is `check-fmt`, `lint`, or `build`. Changed-document Markdownlint and
Nixie also passed. The newest repair wave is formatted and staged. The latest
`make check-fmt` and lint gates passed, with the format log at
`/tmp/check-fmt-2-vtcode-df12-onboarding-test-quality-sweep.out`. The first
post-wave Clippy run found one qualification, which the parent repaired. Build,
harness (49 passed, 2,714 filtered), advisory, and documentation gates also
passed. The optional ast-grep scan was unavailable and was skipped. Four old
rustdoc warnings from the old configuration were accepted; no new warning was
introduced.

The latest full run verified the new parent-death, project-memory, PTY, and
completion-sweep changes. All required gates are now green. Keep the staged
changes uncommitted until the delivery record and PR metadata are finalised.

The recorded 26-failure run covered the sidecar fallback, ACP tool visibility,
prompt-size budgets and golden output, `code_search` replay and loop detection,
command-session fixtures, ANSI snapshots, the delete-file fixture, ambient
steering configuration, provider capability/catalogue assertions, Gemini
thought signatures, and WebMCP digest validation. Do not import the separate
capability train or weaken assertions to hide these failures.

Recent repairs in the resumed wave use owned `TempDir`s, fallible I/O, and
exact delta/snapshot assertions for commons workspace snapshots; shared
round-trip helpers with `#[track_caller]` match-panic assertions for metadata
and tool taxonomy; and exact JSON-field assertions for tool metadata. The ACP
catalogue test now expects the intentional planning list (`list_files`,
`exec_command`), while the production permission gate is unchanged. The
compaction helper now separates pure facts from `#[track_caller]` wrappers and
asserts one envelope index plus the configured minimum retained users; its
continuity tail still checks the full history. The MergeGateway GPT-5.5 entry
was restored from ancestry because the current route contract remains declared.

The resumed remediation wave is formatted and staged, and its required gate
sequence has passed. It covers the stop fixture's
temporary `CODEX_HOME` and configuration-default guard, deterministic pipe
waiting, scoped command policies, fallible ACP and event helpers, prompt-budget
and golden updates, exact ANSI initial-LF handling, coherent Gemini tool-result
continuation, and separating the WebMCP apply/revert workflow from its
missing-helper fail-closed path. The H9 fallback tests passed in the latest
full run.

Recent completion-sweep edits removed inert comments, prints, and empty tests;
replaced TTY smoke tautologies with deterministic capability predicates; made
the core UI renderer check observable text and style; made CLI stub helpers
return contextual results with exact argument, cwd, and error assertions;
removed decorative `Result` plumbing from the theme validity test; and changed
commons diff-wire checks to exact JSON assertions. The most recent
`make check-fmt` passed; its log is
`/tmp/check-fmt-2-vtcode-df12-onboarding-test-quality-sweep.out`. The first
post-wave Clippy run found one qualification, which the parent repaired, and
lint passed. The newest wave also makes startup use a fallible
`ConfigManager` save helper followed by one marker, uses semantically
equivalent prompt assertions with the corrected macOS golden line and missing
batching line, and makes the custom Pro fixture use the existing `ModelConfig`
reasoning override while retaining medium-to-high conversion. The full test
result above passed. Scheduled gates were `make check-fmt`, `make lint`, full
test, then build, harness, optional ast-grep, advisory, and documentation
gates. The required gates passed; optional ast-grep was skipped because the
tool was unavailable. Four old rustdoc warnings from the old configuration were
accepted. PR metadata remains to be finalised.

The hypotheses, evidence, and repair decisions are recorded in
[the debugging record](debugging/debugging-plan-2026-09-05.md). Team context pack
`pk_s2nzbpoo` contains source references and outstanding capability findings.
The separate capability train remains untouched. The complete required gate set
has passed; PR metadata remains to be finalised.

## Resume discipline

Recheck branch, diff, and remote state first. Preserve the current worktree.
Do not reapply scratch lint configuration over these fixes or repeat completed
measurement. Continue the first layer before spelling, structural moves, area
fixes, mechanical nightly formatting, lint configuration, and Whitaker wiring.
Commit each layer only after its full gates pass, then push and open its draft
PR. No later phase has landed yet.

Run gates sequentially through tee; build artifacts use the existing workspace
target directory and shared Cargo cache, never `/tmp`. Nextest is the test
runner; the later explicitly requested doctest gate is the Cargo-test exception.
Use Wyvern for recon and Terra high/Luna xhigh workers as requested. Exchange
team code through context_pack, and use leta, rust-router, and codegraph-mcp.
Load addressing-whitaker-findings for Whitaker fixes and pr-creation per PR.
Global Git attributes configure the Weave merge driver, so inspect future
conflicts for structural corruption.

The local `/home/leynos/.local/bin/ast-grep` failed to execute with
`Permission denied`; it was not retried. Resolve that tool-access issue before
using it for later structural passes. A bounded literal replacement was used
for the 14 identical PTY helper call sites after inspecting them individually.

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
