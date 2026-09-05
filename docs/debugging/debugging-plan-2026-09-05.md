# Debugging plan: ambient configuration in the baseline test

Generated: 2026-09-05. Starting commit: `07e223694`.

The planning agent prepared this document. The initial Wyvern reconnaissance
handled read-only falsification while the session's agent-thread limit
prevented starting an Alchemist. After the restart, Alchemist evaluated H9;
Scrutineer still owns verification gates.

## Observed failure

The unchanged upstream test `test_config_module_integration` creates an empty
temporary workspace, loads configuration, then rejects the loaded provider
`arli`. The full workspace nextest run stopped after 90 passing tests and one
failure. Formatting, Clippy and plain rustdoc had passed.

## H1: workspace isolation leaves global configuration enabled

Claim: `ConfigManager::load_from_workspace` permits user-level configuration
even when its workspace argument names an empty temporary directory.

Prediction: the loader's source shows a reachable user-level configuration
lookup or merge, which can supply `agent.provider` to this test.

Falsification procedure: trace the test's loader call and its configuration
search/merge functions using CodeGraph and live source. The hypothesis is
falsified if every reachable configuration input is confined to the temporary
workspace or immutable defaults. Report the relevant source locations and
whether a public explicit-file or isolated-loading API already exists.

Do not read credentials or print user configuration contents. Do not change
environment variables, source files or the separate capability-hardening train.
Stop with a falsified, not-falsified or inconclusive verdict. If inconclusive,
the planning agent must refine the experiment before source changes.

## Validation after repair

Preserve the test's configuration-loading coverage and assert a deterministic
fixture value. Run the focused integration test, then the full sequential
workspace gates. Do not relax the provider assertion merely to accept ambient
configuration.

## Result

H1 was not falsified. The reconnaissance agent traced the workspace loader to
the system and user layers in
`crates/codegen/vtcode-config/src/loader/manager.rs`. An empty workspace does
not exclude those layers. The observed provider's precise source was not
inspected because the loader trace is sufficient to establish the isolation
gap without reading private configuration.

The fixture now injects the existing public `WorkspacePathsDefaults` provider
with temporary workspace paths and empty home/system search lists. The scoped
test helper restores the original provider, and the test is serialized because
the provider is process-wide. Unlike `workspace.use_root_config`, this avoids
probing user and system files before discarding their layers.

The test checks the exact default provider and tool policy after loading. It
returns `Result` for genuine temporary-directory and configuration I/O, using
fallible checks rather than panics. No production API or loading policy changed.
The final isolation approach passed all 50 focused nextest tests, including
the configuration integration test, event serialization tests and tokenizer
tests. Full workspace validation remains pending.

## Follow-up hypotheses from the complete test run

H2: the image-support tests use dated aliases/display labels that no longer
match the repository's model registry. Two failures reference GPT-5.5 labels.
The reconnaissance agent must trace their fixture construction, alias mapping
and registry entries. Falsify H2 if those exact inputs remain supported by the
current registry and the failure instead comes from runtime capability logic.

H3: the effort-description assertion contains obsolete model-family copy.
It expects `Opus 4.8/5 only`, whereas the returned description says
`Opus 5 only`. The reconnaissance agent must compare the description, supported
effort levels and related model metadata. Falsify H3 if the asserted older
family remains supported and the production description is inconsistent.

These are read-only source checks. Do not change runtime capability behaviour
or import changes from the separate capability-hardening train. Report precise
source evidence before deciding whether either failure is a test-only repair.

H2 was not falsified: both GPT-5.5 fixtures are absent from the current model
registry. The tests now use the registered GPT-5.4 compatibility alias and label.
H3 was falsified: the current presets expose the same model as Opus 4.8 and 5.
The description now names both, preserving the existing assertion.

## H4: cache and watcher fixtures assume distinct filesystem timestamps

The watcher baseline-seed test and primary-memory edit test immediately rewrite
files, including equal-length content. They may retain the same metadata
fingerprint. The prompt-template deletion test may encounter a polling window.

Prediction: cache comparison uses metadata or elapsed time that these fixtures
do not control. Wyvern must trace each failing test and its invalidation path.
Falsify this hypothesis for each test if content is always read or the fixture
already forces a distinct fingerprint and expires the polling window. Identify
an existing clock or metadata injection seam; do not run gates or edit files.

## H5: PTY fixtures inherit command policy before session creation

The shared session lookup sees neither process_id nor session_id. Prediction:
the response reports a policy denial before spawning, rather than a successful
session with missing metadata. Improve the lookup's failure diagnostic, then
have Scrutineer run one failing PTY test. A launch error or successful response
without an ID falsifies the policy-denial hypothesis. Do not change host policy
or disable sandbox enforcement to obtain a passing result.

## H6: provider environment test supplies credentials at the wrong boundary

The Copilot exception test sets command overrides, then sanitization replaces
them with a filtered snapshot of the parent process. Prediction: the asserted
token can only survive when the host happens to export it. Trace the sanitizer
and its callers; falsify H6 if command overrides are part of its documented
input or are preserved by the implementation. If not falsified, identify a
minimal environment-snapshot injection seam that exercises command replacement
without reading or mutating the host environment. Do not inspect secret values.

H4 was not falsified for any of the three fixtures. The watcher compares mtime;
primary memory compares size and mtime; template discovery compares directory
mtime. Tests now set distinct, fixed modification times around their mutations,
including the equal-length memory edit. The existing filetime dependency is
declared directly for these test targets to support directory timestamps across
platforms. No cache invalidation rule or timing budget changed.

H5 was not falsified by the focused nextest run. Its response explicitly denied
`bash -lc 'sleep 0.75; echo done'` under the command execution policy. Tool-level
authorization did not grant command-level permission. The fixture needs its
own explicit command policy; no host policy change is appropriate.

H6 was not falsified. The sanitizer intentionally replaces command overrides
with filtered parent environment entries. A private snapshot-taking function
now supports command-level tests using fixture credentials; the production
wrapper still supplies `std::env::vars_os()`. Assertions check both rejection
of unrelated credentials and retention of the inherited fixture token rather
than a command override. No host credentials are read by these tests.

## H7: parent-death signal test mutates the test harness process

The no-panic parent-death signal test terminates with SIGTERM under nextest.
Prediction: it invokes a process-control operation on the test process itself,
and the helper can deliberately signal that process when the assumed parent
identity does not match. Wyvern must trace the argument and signal path;
falsify H7 if the operation runs only in a fixture child or cannot deliver
SIGTERM to the harness. Identify the smallest child-process fixture that can
verify the intended contract without modifying the test runner's signal state.

## H8: persistent-memory path assertion hardcodes a platform layout

The project-scoped memory test expects a literal state/projects path fragment.
Prediction: the fixture's paths are not injected and the assertion bypasses
the path resolver's platform-specific layout. Trace the resolver and test;
falsify H8 if both already use the same explicit isolated roots. Propose an
exact path assertion built from owned fixture roots, without reading host files.

H7 was not falsified: the old fixture passed getpid where the helper compared
getppid, intentionally triggering SIGTERM in the harness. Two replacement tests
use rusty-fork fixture processes to check successful registration and SIGTERM
on mismatch. They have bounded waits and do not mutate the parent runner's
signal state. Non-Linux platforms retain an explicit no-op assertion.

H8 was not falsified: the original resolver used the host state root. The test
now uses its existing directory-override fixture and compares the exact project
memory path within its temporary workspace; this also avoids probing legacy
host memory during migration.

## Restart checkpoint (completed)

The user requested a stop once the build was stable so Codex could restart with
the increased thread limit. Before the restart, the latest focused run
compiled the workspace and ran 22 selected tests: 21 passed, including H4/H6;
the Gemini thought-signature test still failed its preservation assertion after
replacing its invalid positional lookup. The final checkpoint passed `make
check-fmt`, `make lint`, and `make build`. The staged patch remained
uncommitted because the full layer test gate was unresolved.

Earlier after the restart, `make test` completed 10,085 tests: 10,059 passed,
26 failed, and 17 skipped. The log is
`/tmp/resume-test-vtcode-df12-onboarding-test-quality-sweep.out`. All PTY tests,
H7 parent-death tests, and H8 project-memory tests passed. This is the current
historical result and was superseded by the completion-sweep run below.

## H9 result: sidecar fallback and current model support

H9 was not falsified. `gpt-5-codex` is intentionally not catalogue-supported,
so the sidecar fallback selects the current OpenAI default, `gpt-5.6-sol`,
rather than preserving that unsupported identifier. A worker is adding tests
for preservation of a current supported model and for fallback from an
unsupported model. Runtime fallback behaviour is unchanged.

## Resumed remediation wave

The following work was completed and is covered by the gate results below:

- stop fixture isolation with temporary `CODEX_HOME` and a configuration-default
  guard;
- deterministic pipe waiting and scoped command policies;
- fallible ACP and event helpers;
- prompt-budget and golden updates;
- exact ANSI initial-LF handling;
- coherent Gemini tool-result continuation; and
- separating the WebMCP apply/revert workflow from the missing-helper
  fail-closed path.

Recent repairs in this wave use owned `TempDir`s, fallible I/O, and exact
delta/snapshot assertions for commons workspace snapshots; shared round-trip
helpers with `#[track_caller]` match-panic assertions for metadata and tool
taxonomy; and exact JSON-field assertions for tool metadata. The ACP catalogue
test now expects the intentional planning list (`list_files`, `exec_command`),
while the production permission gate is unchanged. The compaction helper now
separates pure facts from `#[track_caller]` wrappers and asserts one envelope
index plus the configured minimum retained users; its continuity tail still
checks the full history. The MergeGateway GPT-5.5 entry was restored from
ancestry because the current route contract remains declared.

The parent agent also updated the generic raw-extent check to exclude
`CODE_SEARCH`, whose normalized identity already includes its limit and filters;
existing positive coverage and the different-limit negative case remain. The
separate capability-hardening train remains untouched. The required gate set is
now green; PR metadata remains to be finalized.

## Completion-sweep status

Recent edits removed inert comments, prints, and empty tests; replaced TTY
smoke tautologies with deterministic capability predicates; made the core UI
renderer check observable text and style; made CLI stub helpers return
contextual results with exact argument, cwd, and error assertions; removed
decorative `Result` plumbing from the theme validity test; and changed commons
diff-wire checks to exact JSON assertions. The latest `make check-fmt` and lint
gates passed
(`/tmp/check-fmt-2-vtcode-df12-onboarding-test-quality-sweep.out`). The first
post-wave Clippy run found one qualification, which the parent repaired. Build
and harness (49 passed, 2,714 filtered), advisory, and documentation gates also
passed. The optional ast-grep
scan was unavailable and was skipped. Four old rustdoc warnings from the old
configuration were accepted; no new warning was introduced.

The latest completed full suite ran 10,075 tests: 10,075 passed and 17 skipped.
Its log is `/tmp/test-vtcode-df12-onboarding-test-quality-sweep.out`. The ten
earlier failure groups are cleared: seven prompt-wording or golden cases, two
startup duplicate-TOML-table cases during raw serialization and append, and one
Gemini legacy-reasoning case. The two new WebMCP tests, prequeued stop,
compaction, pipe, and PTY cases passed.

The newest repair wave was formatted and staged: startup uses a fallible
`ConfigManager` save helper followed by one marker; prompt assertions are
semantically equivalent with the corrected macOS golden line and missing
batching line; and the custom Pro fixture uses the existing `ModelConfig`
reasoning override while retaining medium-to-high conversion. The full test
result above passed. Scheduled gates are `make check-fmt`, `make lint`, full
test, then build, harness, optional ast-grep, advisory, and documentation
gates. The required gates passed; optional ast-grep was skipped because the
tool was unavailable. Four old rustdoc warnings from the old configuration were
accepted. PR metadata remains to be finalized.
