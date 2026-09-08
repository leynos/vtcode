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
neither prerequisite can satisfy hosted CI alone. The initial CI-foundation
source had exactly fifteen paths: these ten paths plus issue #108's baseline,
checker, checker test, guide and plan, with `.github/workflows/ci.yml` shared.
After hosted validation reached the next Windows warning, root approved one
additional `vtcode-auth` parameter allowance with the exact `not(unix)`
predicate used by its consuming body. Root also approved a deterministic
notices-generator repair after the pinned tool produced different ordering in
CI and restoration of the lost tracked cargo-nextest `ci` profile. Root also
approved the minimal three-file `--print` input-before-startup correction from
issue #114 because an external descendant would leave this foundation PR's
coverage gate red and create a merge-order cycle. There are no API changes,
unrelated dependency updates or new production dependencies in this PR. Never
add blanket cargo-unmaintained or unwrap/expect exemptions to obtain green
checks.

Root authorised one inherited gate-blocker repair after actionlint identified
two unused retry-loop counters in `.github/workflows/build-linux-windows.yml`.
Changing each `for i` to `for _` preserves the twelve-attempt loop behaviour
without expanding the workflow's runtime policy. The historical combined source
had seven tracked modifications, one archive rename and seven untracked
additions: the approved fifteen paths. The current repair adds the narrowly
approved auth, notice-generator, nextest and issue #114 paths. The proposed
tracked source had twenty-one paths before the unwrap enforcement repair. The
seven non-model implementation paths and four model accessor/test paths bring
the proposed tracked source to thirty-two paths. The nextest configuration
remains ignored by default and must be force-added at commit; recount before
staging.

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
- [ ] P2b: Repair the approved hosted Windows and notices failures, rerun
  applicable gates and CodeRabbit, then reach hosted and external-review
  equilibrium before merge. Restore the intended nextest retry profile and
  repair the narrowly traced `--print` startup-order defect under issue #114.
  Complete the thirteen applicable main-baseline production unwrap/expect
  findings without extending the allowlist, then rerun the enforcement gate.
  The model accessor repair must add real GLM-5.3 metadata and preserve the
  fail-loud invariant.
- [x] (2026-09-08) Reconciled the 17-finding enforcement report against the
  actual CI-foundation base. Twelve main-baseline repairs were already in the
  dirty source; the remaining `gatekeeper.rs` capacity invariant is now total
  without an unwrap/expect. Four other reported findings were introduced by
  the unmerged PR #12 and #104 ancestry, so they remain owned by those ACP
  branches rather than being fabricated in this main-based prerequisite.
- [ ] (2026-09-08) Re-run the audit and notices freshness gates after the
  lockfile-only `chacha20` 0.10.2 update. RustSec maintenance debt is tracked
  separately in #119 and #120; no audit warning is allow-listed.
- [x] (2026-09-08) Regenerate `THIRD-PARTY-NOTICES` through the pinned
  repository generator after the `chacha20` lockfile update; the independent
  freshness gate remains pending.
- [x] (2026-09-08) Make the CLI harness fixture hermetic after the current
  full test packet found a host `arli` selection instead of the intended
  OpenRouter authentication control. Focused format, all four harness tests,
  workspace rustdoc, and changed-Markdown checks are green; their logs are
  `/tmp/acp-ci110-hermetic-{fmt,cli-tests,rustdoc,markdown}-1.out`.
- [ ] (2026-09-08) Freeze the candidate for the remaining serial gates. The
  current diff has 24 tracked paths; force-add ignored `.config/nextest.toml`
  as the 25th candidate path before committing. Never stage ignored
  `target-coverage/` or `vtcode-commons.lcov`, which are local coverage
  artefacts only.
- [ ] (2026-09-08) Complete the current-source CI-profile nextest and
  instrumented coverage witnesses, then only the fixture-invalidated Clippy
  and plan-Markdown checks. Existing notices, audit and advisory evidence is
  current for its unchanged inputs. Use hosted CI for Windows; this Linux host
  is not a Windows green witness. Run CodeScene and CodeRabbit only against
  the resulting committed candidate.
- [x] (2026-09-08) Run the exact CI test command in
  `/tmp/acp-ci110-actual-ci-profile-1.out`: 7,294 tests passed under the
  restored `ci` profile, with 16 skipped, one retry-dependent test, and five
  leaky diagnostics. The retry-dependent test is independently tracked by
  PR #117. Four named configuration leaks are recorded by pandalump in
  [#101](https://github.com/leynos/vtcode/issues/101#issuecomment-5584356455);
  their files are unchanged from this candidate's base and their resource
  cause remains unresolved.
- [x] (2026-09-08) Run the workflow-faithful coverage command in
  `/tmp/acp-ci110-full-coverage-1.out`: all 10,054 tests passed, with 17
  skipped; it wrote a 27,668,310-byte `lcov.info` covering 1,624 sources and
  266,574 hit lines. The output is an ignored local artefact, never staged.
- [x] (2026-09-08) Validate the final plan-only Markdown change with pinned
  markdownlint-cli2 0.23.2 in
  `/tmp/acp-ci110-final-fixture-plan-markdown-1.out`.
- [x] (2026-09-08) Replace the eight Clippy-rejected `expect` calls introduced
  by the hermetic fixture with fallible `Result` propagation. Each setup step
  now returns its original error to the test harness; no lint allowance or
  assertion weakening was added. The final packet reruns format, all four CLI
  controls under the CI profile, Clippy, and this plan's Markdown check.
- [x] (2026-09-08) Commit the 25-path CI candidate as
  `2719cde82fe65c12f54dde7714d890e57f879163` after the final formatter gate;
  coverage artefacts remain untracked and excluded from the commit.
- [ ] (2026-09-08) Address the committed candidate's introduced CodeScene
  findings before any CodeRabbit request. The Python baseline checker and the
  changed-Markdown Node helper receive bounded parser/validation decompositions
  with their existing contracts retained. The two Rust safety repairs require
  a separate source review: any complexity reduction must preserve their
  total, fail-closed semantics rather than conceal a branch or fabricate data.
- [ ] (2026-09-08) Re-run only the affected focused checks after the first
  CodeScene repair packet exposed a non-hermetic skills count assertion and
  formatter output. The test must assert exclusion of its invalid fixture
  without treating the deliberately installed system-skill catalogue as empty.
- [ ] (2026-09-08) Commit the final strict-JSON guard repair, then compare the
  immutable head with `dddd1352` in CodeScene. The earlier comparison used
  `2719cde82` and therefore did not include the committed repair
  `26fe6dbdf`; it is not evidence against that repair.
- [x] (2026-09-08) Scope `Seek`, `SeekFrom`, and `Write` imports to the Unix
  compare-and-replace implementation after hosted Windows denied the unused
  imports. `Read` remains unconditional because the bounded-reader helper is
  compiled on every target. This is an import-only platform repair; shared
  validation remains pending.
- [ ] (2026-09-08) Install ripgrep before the CI nextest and coverage jobs.
  Both hosted jobs execute a code-search cache-invalidation contract that
  requests a text result through the external `rg` backend. The runner image
  did not provide `rg`, so the backend was marked unavailable and the contract
  failed before exercising cache invalidation. This follows the existing
  tool-evaluation workflow's explicit installation pattern, retains the
  success assertion, and awaits the focused selector plus hosted reruns.
- [ ] (2026-09-08) Scope four Unix-only test-fixture imports and helpers after
  hosted Windows at `27efb526` confirmed the WebMCP import repair, then reached
  pre-existing `vtcode-core` test warnings under `-D warnings`. The repair
  retains every portable test and Unix symlink/process-group contract; it only
  compiles imports, constants and helpers on the platform of their sole users.
  The next local packet checks formatting, warning-denied core all-target
  Clippy, the affected Unix tests, this plan's Markdown and spelling. Fresh
  hosted Windows remains the authoritative target-platform witness.
- [ ] (2026-09-08) Make the PreToolUse rewrite fixture consume the hook JSON
  payload before emitting its unchanged response. Hosted coverage first failed
  at `pre_tool_use_hook_rewrite_reaches_approved_args`: the expected rewrite
  was absent after the print-only child could exit before the engine completed
  its stdin write. This is a source-backed fixture-race inference, not a claim
  that the hosted log captured a broken-pipe diagnostic. Existing #54 and #92
  own the related runtime early-stdin-close behaviour; PR #117 applies the
  same fixture-drain pattern elsewhere and does not own this file.
- [ ] (2026-09-08) Retain the attached `-o` flag with the `out` filename in
  the shell-intent mutation fixture while clearing the focused spelling gate.
  The repository has no tracked spelling configuration, so a global dictionary
  exemption would broaden unrelated checks. A `concat!` literal preserves the
  exact runtime command while presenting the two ordinary lexical tokens
  separately to the checker.

## Surprises & discoveries

PR11 evaluation fails compilation because its ACP checkpoint initializer
omits optional turn_diagnostics. Root already prepared that one-field repair
in the separate PR11 worktree. It is not a failure on this main-based branch.

PR11 coverage fails before running tests: cargo llvm-cov --no-run attempts to
merge absent profraw data. The latest baseline still uses that command. The
Windows native_roots home_dir parameter and non-Unix set_private_permissions
path parameter produce deny-warnings failures on Windows. The stable handoff
document remains outside scripts/docs_top_level_allowlist.txt.

After the WebMCP `filesystem.rs` import repair passed in the Windows job,
Windows compiled far enough to expose test-only Unix dependencies in
`vtcode-core`. The four affected modules are byte-identical between merged
baseline `dddd1352`, prior PR #110 head `8b98cddf`, and `27efb526`; this is
pre-existing conditional-fixture debt rather than a regression from the
foundation work. The source repair gates only items whose existing sole callers
are already `#[cfg(unix)]`; it does not suppress warnings or remove portable
test coverage.

Hosted coverage at `27efb526` no longer failed in the external `rg` backend,
but its first subsequent failure was the PreToolUse rewrite fixture. The engine
awaits its stdin write before collecting child output, while that fixture used
only `printf` and could close stdin first. The resulting engine error is
handled as a hook diagnostic and the permission path proceeds without a
rewrite. The log records the missing rewrite rather than a broken-pipe error,
so the causal mechanism is source-backed inference. Draining stdin in the
fixture before printing identical JSON is the established test pattern; #54
and #92 retain ownership of the production early-close boundary.

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

The current audit also reports yanked `chacha20 0.10.1` through `rand 0.10.2`.
The crates.io index resolves the direct compatible replacement `chacha20 0.10.2`
without a manifest change; Cargo updated only the lockfile version and checksum.
The remaining unmaintained advisory paths are tracked separately: #120 owns
`syntect 5.3.0`'s `bincode 1.3.3` and `yaml-rust 0.4.5` paths, and #119 owns
`image 0.25.10`'s `paste 1.0.15` backend paths. Neither is suppressed or folded
into #108's cargo-unmaintained baseline ratchet.

The upstream EmbarkStudios cargo-about 0.9.1 release supplies an
x86_64-unknown-linux-musl archive and SHA-256 sidecar. Its published checksum
verified before generation. A first command was rejected before execution
because its cleanup trap used `rm -rf`; the replacement used a retained
`mktemp` scratch directory and the verified archive directly. It did not
compile or install a Cargo tool. After PR #110's hosted notice check disagreed
with that output, a retained official archive again verified as
`c0e7dc6f5d74b0beec5c0053d39ab24514c717d19acd91886907a22457ea9e98`, with
binary SHA-256 `c6e1f29e4ef8b34eab4689a6295ac42eb0063cff8699fad90bb0beceb48d19e9`.
The CI log shows the pinned install action falls back to cargo-binstall and
obtains the x86_64 musl package from GitHub. The template exposes no explicit
sorting; source investigation and a repeat-generation witness must therefore
precede a generator repair.

Cargo-about 0.9.2 is the smallest upstream release that fixes the relevant
license-text preservation defect. Its official x86_64 musl archive verified
as `9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af`, and
the extracted binary verified as
`b06bd6a8bfd726cffb90e3e0588e3e0b1cfbb582bf6a34f4c1c2692ba8f2e7b8`.
Two generation runs with that binary produced byte-identical notices with
SHA-256 `8bdd004658b7eb48be49481ca5790e8fdfc13ca7c02debdb7a0af3554429474d`;
two subsequent `--check` runs passed. The generator now requires that exact
tool version and retains generator diagnostics instead of discarding stderr.

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

The final combined deterministic packet was green before hosted validation.
Hosted PR #110 then exposed two further CI-foundation concerns: the approved
common-platform repair let Windows reach an unused
`enforce_private_permissions` parameter in `vtcode-auth`, while the hosted
notice generation placed `libmimalloc-sys` under a different MIT grouping than
the separately verified local generator. The production unwrap enforcement
fails identically on main with twelve unrelated paths and remains outside this
PR; no allowlist expansion is authorised. The next packet verifies the two
approved repairs before another review.

When the refreshed hosted Windows job reached `vtcode-core`, root approved the
smallest platform-scoped fixture correction: imports and helpers used only by
existing Unix tests must themselves be `#[cfg(unix)]`. This retains the tests
on Unix and leaves portable test cases compiled on Windows. No broad test
skipping, `allow(unused)`, `allow(dead_code)`, public API, or runtime behaviour
change is permitted.

Root also approved the one-line PreToolUse fixture drain after the hosted
coverage failure: `cat >/dev/null` must consume the engine payload before the
fixture emits the same `updatedInput` JSON. The changed test continues to prove
the existing rewrite contract and does not change the production early-close
path tracked by #54 and #92. This follows PR #117's fixture pattern without
duplicating that PR's different files.

The focused spelling packet found one prose correction and one deliberate
lexer false positive. The prose now uses the accepted adjective. The test must
retain the attached `-o` flag with its `out` filename because it is an
adversarial mutation case; compile-time string concatenation produces the same
command without adding a repository-wide spelling exemption.

Hosted-green delivery requires the twelve production unwrap/expect findings as
well. The inherited enforcement report identifies twelve findings across ten
implementation files: three model metadata accessors, two session helpers,
skill discovery, OpenAI response history, three TUI helpers, and CLI stdin
preparation. Root requires direct fallible or total replacements rather than
an allowlist. The repairs preserve valid outputs, turn invalid lifecycle/input
states into results, and keep optional UI enhancement failure nonfatal.

The only model whose generated/table metadata misses a built-in variant is
`OpenRouterZaiGlm53Flash`. Its exact canonical values are API id
`z-ai/glm-5.3-flash`, display `GLM-5.3 Flash`, and description
`Z.AI GLM-5.3 Flash efficient multimodal model via OpenRouter`. A fabricated
fallback would risk a wrong external request, so the accessors add this real
manual mapping. Every residual impossible built-in case uses the explicit
fail-loud `unreachable!("built-in model missing generated or table metadata")`
instead of repeating the metadata lookup. Direct exact regression and
all-model accessor coverage prevent a future table omission from hiding.

Hosted Cargo nextest also fails before compiling tests because the workflow
selects `--profile ci` but the repository does not track `.config/nextest.toml`.
The existing Cargo `[profile.ci]` is distinct from nextest's test profile.
Historical source records the intended test policy as two retries, no
fail-fast, failing-test status output and flaky final status. Restore only that
explicit nextest profile, rather than dropping the workflow argument and
silently losing retry policy.

The coverage job now executes tests, then exposes a product defect:
`vtcode --print` without a prompt resolves provider startup before it validates
input, so an unrelated missing OpenRouter credential masks `No prompt
provided`. A fake test API key would conceal the ordering defect. Correcting
the input-before-startup sequence crosses into runtime dispatch. Root approved
the exact three-file correction under issue #114 as an exception to this
foundation PR's ordinary CI-only boundary, because a separate descendant would
leave its coverage gate red and introduce a circular merge dependency.

The prior final combined deterministic packet passed the pinned
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
the component's hosted CI to pass. Initial scope was limited to the fifteen
verified combined paths; root later approved the bounded hosted repairs and
issue #114 exception described above. Existing baseline and capability fixes
retain their own delivery provenance. Plans are approved under the user's
authorization; workers do not request per-milestone user approval.

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
provenance. Its five unique paths joined this component's ten paths; the shared
CI workflow made fifteen initial paths. The issue's baseline debt remains open
and visible; this decision changes delivery ordering only.

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

Root approved restoration of the missing nextest `ci` configuration after the
hosted job exited 96 with only `default` and `default-miri` known. The forced
tracked configuration restores the historical retry policy. It does not alter
the existing Cargo build profile or remove the workflow's `--profile ci`
argument.

Issue #114 owns the runtime-correctness invariant. Its expected user contract
is that `vtcode --print` without inline or piped input reports the prompt error
without requiring any provider credential. The minimal repair is delivered in
this PR because it is required for the hosted coverage gate. It must not fake
an API key or otherwise hide the defect.

The #114 implementation resolves the existing print helper before
`StartupContext` construction, carries its resulting prompt through dispatch,
and does not reread stdin. The no-input harness removes `OPENROUTER_API_KEY`
and rejects provider-auth diagnostics; a present-inline-input harness removes
the same key and requires the provider-auth diagnostic. Both are offline
negative controls and make no provider request.

The unwrap enforcement repair is deliberately local: guarded non-negative
integer conversion uses a total cast; JSON `Value` uses its total display
serialization; missing skill manifests are skipped directly; and the already
non-empty tool-call identifier is retained by the match. Terminal restoration
now returns a contextual error if disarmed; regex construction makes compaction
unavailable instead of panicking; impossible empty replacement parts return
before input mutation; and required stdin absence returns the existing prompt
error. No warning suppressions or default fabricated values are introduced.

For the gatekeeper cache, the exact 1024-entry capacity now uses the safe const
construction `NonZeroUsize::MIN.saturating_add(1023)`. It retains the exact
capacity and establishes non-zero at compile time, without an unwrap, expect,
runtime panic, or unsafe precondition.

Cargo updated only `chacha20` from 0.10.1 to its unyanked, Rust-1.85-compatible
0.10.2 release. This satisfies `rand 0.10.2`'s existing `^0.10.0` requirement;
the follow-up audit and notice freshness checks remain required before publish.
The separate, non-suppressed RustSec remediation work is #119 and #120.

The final compile repair removes an unnecessary crate qualification in the
binary's local `main_helpers` call and uses `isize::cast_unsigned()` after the
existing `delta >= 0` guard. The latter is stable since Rust 1.87, remains below
the 1.88 MSRV, and removes the checked-cast lint without weakening the guard.

The later full test batch passed 10,091 tests, skipped 17, and failed only
`print_mode_with_prompt_requires_provider_authentication`: host configuration
selected `arli` despite the fixture's intended OpenRouter assertion. The
fixture now writes a root-only workspace configuration, passes the explicit
OpenRouter provider/model flags, clears inherited environment credentials, and
uses private HOME, VTCODE, and XDG roots under `TestHarness`. This keeps the
missing-prompt/authentication ordering controls offline and deterministic; it
does not broaden the expected provider error. The same batch's four rustdoc
warnings are repaired by making the two private UI references plain code,
linking the public trait method through `Self`, and making the private
`close_tree` reference plain code. Focused format, rustdoc, changed-Markdown,
and all four CLI harness controls now pass. The only failed test was fixed
without broadening its OpenRouter assertion. The exact CI-profile and coverage
commands still require a fresh serial run because they exercise the full
current test graph and the instrumented workflow path.

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

The committed candidate's CodeScene comparison against `dddd1352` found newly
introduced complexity and test-structure findings in the PR-owned baseline
ratchet and changed-Markdown helper. Root approved coherent helper extraction
and contract-preserving test consolidation for those scripts; no CodeScene
threshold change, suppression, or diagnostic relaxation is permitted. The
same comparison reports complexity increases in `ListSkillsTool::execute` and
`InputManager::replace_range` from prior `expect` removals. Those functions
remain safety-sensitive: their follow-up must demonstrate a meaningful total
representation or decomposition before changing code. The 1,025-line
`input.rs` result includes this candidate's one-line optional-regex guard after
an existing 1,024-line baseline. It remains tracked by #118 because reducing
the module below 1,000 lines requires unrelated pre-existing decomposition;
this is a reasoned structural follow-up, not a waiver of the new guard.

The skills loop now uses `filter_map` to state its actual domain: only skill
metadata with a manifest reaches filtering and presentation, while discovery
errors remain reported separately. The input replacement derives its final
segment with `rsplit_once`, which is total for empty, single-line and
trailing-newline strings and keeps every existing range and multiline branch.
A direct Unicode/trailing-newline replacement test records that behaviour.
The GLM-5.3 Flash accessor example moved beside the manual `ModelId` accessors,
so `models/tests.rs` returns to its prior responsibility count without
weakening all-model metadata coverage.

The final CodeScene pass over `26fe6dbdf` identifies three strict decoded-JSON
integer guards as two-branch conditionals. Each now uses `type(value) is not
int` before its existing value predicate, which rejects JSON booleans and
floats without a special boolean branch or a schema relaxation. One focused
subtest-driven control now covers boolean and float rejection for age,
schema-version and issue, retaining each field-specific diagnostic.
`ListSkillsTool::execute` retains its baseline cyclomatic complexity of 12; its
three added lines are formatter layout for the manifest-backed iterator, with
no new decision path. The changed-Markdown helper has no complex method above
threshold; its 4.78 aggregate reflects nine explicit JSON, path, filesystem,
symlink and child-process fail-closed functions. Splitting those functions
again would only change the denominator and weaken local boundary legibility.
These are documented scope dispositions, not CodeScene suppressions. The
`input.rs` 1,024-to-1,025 safety guard remains separately tracked by #118.

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
toolchain supports it. The actual main Makefile is the required full release/PR
route. Run
`make -j1 typecheck`, then `make -j1 check-fmt`, followed by
`make -j1 lint build test test-harness advisory`, with `RUSTFLAGS=-D warnings`
and the shared target directory. This is the exact `make check` sequence with
only `check-ast-grep` omitted: its discovered executable is mode 0600 and root
explicitly prohibits invoking or bypassing it. This does not claim that the
optional scan passed. Do not claim hosted Windows coverage from Linux-only
checks. Record any environment prerequisite precisely rather than silently
skipping a gate.

Before the full packet, run the two #114 harness tests and focused model,
core, LLM and TUI regression tests with the restored nextest `ci` profile and
Cargo `ci` build profile. They must show that no input
returns the prompt diagnostic without OpenRouter authentication, while an
inline prompt reaches the existing missing-provider diagnostic. This is a
local, no-network contract witness. The full CI nextest invocation must then
recognise the `ci` profile and retain its two-retry, no-fail-fast policy.

Coverage's negative control is the archived hosted failure before any test
execution. The corrected path must execute instrumented nextest tests and
produce non-empty lcov data. A textual command replacement alone does not
prove the coverage workflow. The fixed advisory must disappear from the
resolved lockfile check without suppressing it. Licence regeneration must be
idempotent. Windows compilation is the witness for platform warning removal.

For the Unix-only fixture correction, the serial packet is limited to
`cargo fmt --check`, warning-denied `vtcode-core` all-target Clippy, the two
apply-patch symlink contracts, the Unix-only shell-intent assertion, and the
two PTY process-group contracts, followed by pinned Markdown and spelling for
this plan. Do not retry this Linux host's direct Windows cross-check: its
`aws-lc-sys` cross-environment failure is non-decisive. The refreshed hosted
Windows workflow is the target-platform acceptance proof.

The same serial packet runs the repaired root-binary hook fixture with
`-p vtcode --bin vtcode` under the CI nextest profile and under scoped
instrumented coverage, so neither command builds the default-member test
inventory for this one contract. The test keeps its existing timeout and
rewrite assertion. Its adjacent forwarded-phase control remains in the normal
focused selector because it proves the distinct no-double-invocation path.

Changed-Markdown validation first runs `node --test
scripts/tests/lint_changed_markdown.test.mjs`. Its contract covers JSON parsing,
non-empty input, literal `--` argv construction, hostile filenames, preserved
child status, regular-file checks and realpath containment. A focused helper
run then uses the actual pinned CLI 0.23.2 against each new or modified
Markdown path; changed Markdown must be green. This is incremental coverage,
not evidence that the baseline full-repository scan tracked by issue #107 is
green.

### Frozen current-source gate packet

The final focused repair changed only the CLI test fixture and four public-doc
lines after the prior broad packet. The focused evidence above is therefore
current for format, the four affected tests, rustdoc, and changed Markdown.
The earlier locked typecheck, ordinary build, and focused harness evidence is
reusable for unchanged production source; do not repeat it merely to create
duplicate logs.

The shared scrutineer ran the commands sequentially with tee logs after the
candidate was frozen:

1. `cargo clippy --locked --workspace --all-targets --all-features --jobs 6 --
   -D warnings` with `RUSTFLAGS=-D warnings` and the shared target directory.
   The current CI-profile test command does not lint the changed Rust test
   fixture, while shell, policy, production Clippy and rustdoc inputs are
   unchanged since their green runs. The corrected invocation is active in
   `/tmp/acp-ci110-final-fixture-clippy-2.out`; no source failure has been
   observed.
2. `cargo nextest run --locked --profile ci --cargo-profile ci`. This is the
   exact CI test-job command and the required full current-source test witness;
   it confirmed the restored `ci` nextest profile rather than the local default.
3. `CARGO_INCREMENTAL=0 cargo llvm-cov nextest --locked --workspace --lcov
   --output-path lcov.info`. This workflow command passed all 10,054 tests
   and emitted the non-empty LCOV witness recorded above. `lcov.info`,
   `target-coverage/`, and `vtcode-commons.lcov` remain untracked artefacts.
4. The pinned changed-Markdown CLI passed for
   `docs/execplans/ci-prerequisites.md`; no other Markdown source changed
   after the focused Markdown evidence.

No notices or audit rerun is needed: both notices `--check` passes occurred
after the `chacha20` update and generation, and
`/tmp/acp-ci110-repair-audit-1.out` is the post-update audit witness. No
advisory rerun is needed: the current production unwrap enforcement passed,
the modified harness path is excluded from that scanner, it remains below the
500-line file-length threshold, and the allowlist and legibility inputs are
unchanged. The three unmaintained warnings remain tracked by #119 and #120;
the yanked chacha20 warning is absent from the post-update audit.

After the active Clippy run is green and the candidate is committed, run
`cs delta dddd1352fcdf41e86abe380423221f4b41f171e0 <candidate>` through the
shared runner, request CodeRabbit only after the deterministic packet is green,
and retain hosted CI as the Windows and final coverage authority.

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
rerun was required before the prior draft commit. The current thirty-two-path
repair requires a fresh review after its own deterministic packet.

Read-only GitHub inspection confirms draft PR #110 still points at prior head
`e5a091ece8510315e942a210df61a2e62e1e0fed`. Its completed failure inventory is
limited to coverage, unwrap enforcement, licence notices, Windows, and nextest;
all are from that old source and have scoped WIP repairs. No current job is
running. Do not use those old results as validation for the WIP.

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
THIRD-PARTY-NOTICES was regenerated with verified cargo-about 0.9.2 output.
The frozen component was applied unchanged to clean reviewed ACP candidate
`21b24eaeacac7c5595477ebf5da94b9c299121b3`; only its matching Rust attribute
layout is retained here. The reviewed #108 ratchet now joins it as one
initial fifteen-path CI-foundation source. Initial scoped evidence passed: diff
checks,
changed-Markdown helper contracts and selected-file lint, documentation
location/links, workflow security, and corrected actionlint. Commit
`e5a091ece8510315e942a210df61a2e62e1e0fed` and draft PR
[#110](https://github.com/leynos/vtcode/pull/110) now carry the source. Hosted
CI, external review equilibrium and merge remain pending. Hosted Windows
validation exposed the approved `vtcode-auth` allowance after the original
common-platform repair passed; hosted notice ordering requires the approved
deterministic-generator repair. Hosted nextest requires the restored `ci`
profile, and coverage requires the issue #114 input-before-startup repair.
The enforcement gate requires the direct thirteen-finding main-baseline repair
and its model-table regression coverage. Four additional report rows arose in
unmerged ACP PR #12/#104 source and require their owning-branch repairs, not
duplicate CI-foundation changes. The previous staged CodeRabbit review returned
zero findings; the repaired thirty-two-path source requires a fresh review
after its applicable gates pass.

Revision note: Initial approved packet separates shared CI repairs from the
Wave 1 runtime defects and records exact negative controls and scope limits.
