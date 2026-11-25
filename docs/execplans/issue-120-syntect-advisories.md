# Remediate syntect's unmaintained transitive dependencies

Status: COMPLETE — implementation, affected-crate gates, and the
workspace-wide locked gate run all pass. This prerequisite addresses issue
[#120](https://github.com/leynos/vtcode/issues/120).

## Purpose and constraints

`cargo audit --file Cargo.lock` reports two informational "unmaintained"
warnings that reach Diapason only through the UI syntax-highlighting
dependency. Both paths are identical:

```plaintext
diapason-ui -> syntect 5.3.0 -> bincode 1.3.3     (RUSTSEC-2025-0141)
diapason-ui -> syntect 5.3.0 -> yaml-rust 0.4.5   (RUSTSEC-2024-0320)
```

Neither crate is reachable at runtime in a way that parses untrusted input:
`bincode 1.3.3` decodes the bundled `.packdump` and `.themedump` assets that
ship inside syntect itself, and `yaml-rust 0.4.5` is only pulled in by syntect's
`yaml-load` feature, which Diapason never calls. The requirement is still to
remove both advisory paths without suppressing them, because the RustSec gate
must stay honest and because future Diapason work could make the YAML path
reachable.

Two independent causes need two independent fixes:

- The `yaml-rust` path exists only because `diapason-ui` enables syntect's
  `default-fancy` feature bundle, which includes `yaml-load`. Diapason only
  uses the bundled dumps plus regex highlighting, so narrowing the feature list
  removes the path outright, with no source change.
- The `bincode` path is unavoidable while syntect 5.3.0 is the dependency:
  `dump-create` and `dump-load` both list `bincode` as their codec. Upstream has
  not released a replacement. PR
  [trishume/syntect#694](https://github.com/trishume/syntect/pull/694) swaps the
  codec for `serde-wincode` but has sat unreviewed since June 2026, and the
  `syntect 6.0.0` milestone it targets is still open. Diapason therefore vendors
  syntect 5.3.0 through the root `[patch.crates-io]` table and changes only the
  codec, to `bincode-next 3.1.2` in its `legacy()` configuration, which
  reproduces bincode 1.3's wire format.

Hard invariants for this work:

- Syntax highlighting behaviour is preserved: the same bundled grammars, the
  same seven themes, the same `SyntaxSet`/`ThemeSet` public types, the same
  scopes, and the same colours reach the UI.
- The bundled `.packdump` and `.themedump` assets are *not* regenerated. They
  must decode byte-for-byte with the new codec, otherwise the fork is wrong.
- Nothing suppresses a RustSec advisory. No `--ignore`, no `[advisories]`
  ignore list, no `cargo-deny` exception, no advisory-db edit.
- Builds stay locked (`--locked`). Unrelated dependency upgrades are excluded
  from the change.

## Context and orientation

Diapason is a hard fork of VTCode: a Rust 2024 workspace at MSRV and pinned
toolchain 1.93.0, whose default members are the root `diapason` binary,
`crates/codegen/diapason-core`, and `crates/codegen/diapason-ui`.

Syntax highlighting lives in one module,
`crates/codegen/diapason-ui/src/tui/ui/syntax_highlight.rs`. It holds a
process-global `SyntaxSet` built by `SyntaxSet::load_defaults_newlines()` and a
`ThemeSet` built by `ThemeSet::load_defaults()`, both of which decode dumps
embedded in syntect's own binary. The module then highlights with
`syntect::easy::HighlightLines` and converts `syntect::highlighting::Style`
into `anstyle` and `ratatui` styles. Consumers are
`src/agent/runloop/unified/tool_summary.rs` (bash command preview),
`src/agent/runloop/unified/tool_pipeline/pty_stream/segments.rs` (PTY output),
`src/agent/runloop/tool_output/streams.rs` (diff prose), and the in-crate
`tui::ui::markdown` and `tui::core_tui::log` renderers.

The dependency is declared once, at
`crates/codegen/diapason-ui/Cargo.toml:48`:

```toml
syntect = { version = "5.2", default-features = false, features = ["default-fancy"] }
```

`crates/codegen/diapason-ui/Cargo.toml:5` already pins `rust-version = "1.93.0"`,
as does the workspace. `bincode-next 3.1.2` declares `rust-version = "1.90.0"`,
which is below that floor, so no MSRV change is implied by the fork.

The root `Cargo.toml` already carries a `[patch.crates-io]` table that redirects
the workspace's own crates to their paths, plus one true third-party patch:

```toml
simdutf = { path = "patches/simdutf" }
```

`patches/simdutf/` is a cargo-normalized vendor of an upstream crate with its
`LICENSE` and sources retained. `patches/syntect/` follows that layout and that
precedent.

### Survey of affected surfaces

The change touches, at most:

- `crates/codegen/diapason-ui/Cargo.toml` — feature narrowing (one line).
- `patches/syntect/` — a new vendored crate tree.
- `Cargo.toml` — one added `[patch.crates-io]` line.
- `Cargo.lock` — the resolution consequences of the two above.
- `THIRD-PARTY-NOTICES` — licence inventory.
- `crates/codegen/diapason-ui/src/tui/ui/syntax_highlight.rs` — one added
  regression test.
- `scripts/cargo_unmaintained_baseline.json` — removal of entries that the
  scanner no longer reports.
- `docs/execplans/issue-120-syntect-advisories.md`,
  `docs/modules/diapason_ui.md`, and `docs/execplans/ci-prerequisites.md` —
  documentation.

No comparable ExecPlan in this repository records a dependency-vendor change,
so there is no local historical budget evidence to cite. The nearest analogue,
`docs/execplans/issue-108-unmaintained-baseline.md`, covers six paths. The
budgets below are therefore first estimates and are treated as escalation
thresholds, not entitlements.

## Conformance basis

The governing artefact is issue #120 itself, whose acceptance criteria are
quoted in `Purpose and constraints`. Its sibling
`docs/execplans/ci-prerequisites.md` records the ownership split: #120 owns the
two `syntect` paths, #119 owns `image`'s `paste` paths, and #108 owns the
`cargo-unmaintained` scanner baseline. That split is a hard boundary here: this
plan must not add or refresh scanner baseline entries beyond deleting the two
identities it removes.

Propositions traced through the plan:

- `EP120-REQ-1`: a maintained dependency or configuration removes both
  advisory paths, with release notes and compatibility constraints recorded.
- `EP120-REQ-2`: the change is the smallest compatible dependency or source
  change, with no unrelated upgrades bundled in.
- `EP120-REQ-3`: `cargo audit --file Cargo.lock` no longer reports
  `RUSTSEC-2025-0141` or `RUSTSEC-2024-0320`.
- `EP120-REQ-4`: affected UI syntax-highlighting tests pass, along with locked
  check and lint gates.
- `EP120-REQ-5`: highlighting behaviour, locked builds, and all supported UI
  paths are preserved — the plan's own statement of the issue's intent.

## Constraints

- Do not add a RustSec ignore, allow-list, or advisory-database edit anywhere.
- Do not regenerate syntect's `.packdump` or `.themedump` assets, and do not
  change the `SyntaxSet`, `ThemeSet`, `SyntaxReference`, `Style`, or `Theme`
  types.
- Do not change the wire format of `SyntaxSet`'s nested
  `serialized_lazy_contexts` blobs. They are dumped and reloaded by syntect
  itself at runtime, so the change must be format-preserving in both
  directions, not merely decode-compatible.
- Keep `--locked` builds working and keep `Cargo.lock` changes limited to the
  packages this change adds or removes.
- Do not upgrade unrelated dependencies, including `flate2`, `fancy-regex`,
  `regex-syntax`, `onig`, or anything else syntect depends on.
- Do not widen the change to `image`, `paste`, `chrono`, `humantime`, `zbus`,
  or the other advisories currently reported. They belong to #119, #108, or
  other work.
- Keep the vendored crate free of `#![allow(...)]` blanket suppressions; it must
  compile clean under the workspace's warning policy.

## Tolerances (exception triggers)

- Code: at most 40 net changed lines outside `patches/syntect/`, excluding
  ExecPlan and generated-notice files. The fork's own tree is capped at 120
  net changed lines relative to the published 5.3.0 archive, because only the
  codec changes.
- Configuration: at most 120 net changed lines, of which `Cargo.lock` may
  account for at most 100. A larger lockfile delta means the resolution moved
  more than this change should justify — stop and inspect.
- Documentation: at most 260 net changed lines excluding ExecPlan files.
  ExecPlan files are excluded from every budget above.
- File count: at most 10 changed files outside `patches/syntect/` and the
  ExecPlan.
- Advisory scope: if `cargo audit` grows by any advisory other than the two
  removals, stop and report.
- If `bincode-next` fails to reproduce the bundled dumps byte-for-byte, stop
  and escalate rather than regenerate assets or switch serializer.

## Risks

- Codec incompatibility in the wild. `bincode 1.3.3`'s
  `serialize_into`/`deserialize_from` are fixed-int, little-endian, and permit
  trailing bytes. `bincode-next`'s `legacy()` must match all three. Mitigated
  by a byte-identical decode/re-encode test over all bundled assets, run before
  any UI code is touched.
- Silent behavioural drift. A codec that decodes without error but reorders a
  context map or drops the regex cache could change highlighting subtly.
  Mitigated by a full-syntax and full-theme regression test, plus the existing
  theme and renderer tests.
- Downstream publication. `[patch.crates-io]` does not travel to consumers of a
  published crate; a published `diapason-ui` would build against upstream
  syntect again. This is acceptable because the advisory is informational and
  the workspace is the delivery vehicle, but it is recorded rather than
  glossed.
- Upstream drift. When syntect publishes a release that replaces the codec, the
  fork should be deleted. Recorded as an exit path rather than a permanent
  commitment.
- Compile-admission queueing on this host. Gates may queue behind other agents
  and run slowly; that is not a failure of this change.

## Verification plan

Invariants and their discharge:

1. The bundled assets decode and re-encode byte-identically under the new
   codec, in both syntect dump formats. Method: a one-shot verification program
   (`/tmp/issue120-probe`) outside the repository, decoding every asset through
   the vendored syntect and re-encoding it with both the vendored codec and
   stock bincode 1.3.3, then comparing bytes. Non-vacuity: the same program also
   asserts that a byte-truncated dump fails to decode, so a codec that accepted
   everything could not pass. Discharged: `WIRE COMPATIBILITY: PASS`, 82
   checks, transcript below.
2. Every bundled syntax's nested lazy-context blob decodes; every syntax
   highlights a sample; the decoded grammar assigns the scopes the theme
   expects; every bundled theme decodes into usable scope rules. Method: three
   named regression tests in the existing `tests` module of
   `crates/codegen/diapason-ui/src/tui/ui/syntax_highlight.rs`.

   Non-vacuity, test by test. `every_bundled_syntax_highlights_a_sample`
   reconstructs the source text from the returned segments and asserts the
   bundled set exceeds 50 grammars before iterating; starting a parser at all
   requires `SyntaxReference::context_ids()`, which decodes the nested blob
   through `LazyContexts::deserialize`, so a broken nested codec fails on the
   first line rather than passing quietly. Reconstruction alone proves only
   that characters were not dropped, so
   `bundled_rust_grammar_assigns_expected_colours` pins the actual
   base16-ocean.dark foregrounds for a Rust keyword, a numeric literal, a plain
   identifier, and a comment, and asserts they are mutually distinct. That
   test was checked against a deliberately wrong expected colour and failed
   (exit 100), which is the negative control for scopes and colours rather than
   for text. `every_bundled_theme_loads_by_name` asserts each theme is present
   in the decoded dump and carries a non-empty rule list: `load_theme`
   silently falls back on an unknown name, so the name alone was not evidence,
   and an earlier draft comparing a value with itself was vacuous.

   Discharged: 3/3 for the new tests, 19/19 for the whole
   `test(syntax_highlight)` filter.
3. `cargo audit --file Cargo.lock` reports neither `RUSTSEC-2025-0141` nor
   `RUSTSEC-2024-0320`, and no new advisory. Method: direct command, compared
   against the recorded 12-warning baseline. Discharged: 10 warnings, the two
   targets absent, nothing added.
4. No dependency on `bincode` or `yaml-rust` remains. Method:
   `cargo tree -i bincode` and `cargo tree -i yaml-rust`. Discharged: both
   report "did not match any packages"; `cargo tree -i bincode-next` shows one
   path through the vendored syntect.
5. Locked check and lint gates stay clean. Method:
   `cargo fmt --all -- --check`, `cargo check --locked`, and
   `cargo clippy --locked -- -D warnings`. Discharged for the affected crate
   with `RUSTFLAGS="-D warnings"`; the workspace-wide run is the serial gate
   runner's evidence.

Third-party interfaces are treated as axioms: syntect's documented dump
functions, `bincode-next`'s documented `legacy()` format, and `flate2`'s zlib
format. The repository-owned logic that needs evidence is the adapter code that
joins them, which invariants 1 and 2 exercise against the real interfaces.

## Milestones

Milestone 1 — establish and verify. Capture the audit baseline, both inverse
dependency trees, and the focused UI test result. Confirm no published syntect
release removes either advisory crate. Verify `bincode-next`'s maintenance
status, licence, MSRV, transitive dependencies, RustSec status, and legacy
format semantics. Record all of it here.

Milestone 2 — narrow features and vendor the fork. Replace `default-fancy` with
the exact features Diapason uses. Create `patches/syntect/` from the verified
5.3.0 archive, change only the codec, and add the root patch entry. Resolve
once, unlocked, and accept only the intended lockfile delta.

Milestone 3 — prove compatibility. Run the byte-identical dump check, then add
the bundled-asset regression test.

Milestone 4 — validate and record. Run the affected UI tests and the locked
check/lint gates, regenerate notices, remove now-stale scanner baseline entries,
run `cargo audit`, and finish the documentation.

Each milestone ends in a state that can be committed and continued from.

## Outcomes & retrospective

Both advisory paths are gone and no advisory is suppressed.

`cargo audit --file Cargo.lock` (cargo-audit 0.22.2, 1566 advisories) went from
12 allowed warnings to 10. The two that disappeared are exactly
`RUSTSEC-2025-0141` and `RUSTSEC-2024-0320`; nothing new appeared.
`cargo tree -i bincode` and `cargo tree -i yaml-rust` now both fail with
"did not match any packages". `cargo tree -i bincode-next` shows a single path:
`bincode-next -> syntect -> diapason-ui`.

The lockfile delta is 22 insertions and 37 deletions, and every hunk is one of
four expected kinds: `bincode 1.3.3` replaced by `bincode-next 3.1.2`; `syntect`
losing its registry source and checksum and gaining `bincode-next` while losing
`bincode`, `plist`, `serde_json`, and `yaml-rust`; the three crates that only
existed for syntect's dropped features (`plist`, `yaml-rust`,
`linked-hash-map`) removed; and the three new `bincode-next` dependencies
added (`rapidhash`, `unty-next`, and the already-present `pastey 0.2.3`). No
other package changed version.

Syntax highlighting behaviour is preserved by construction and by test. The
bundled assets are the published ones, unchanged, and `diapason-ui`'s public
types (`SyntaxSet`, `ThemeSet`, `Style`, `Theme`) are syntect's own. The two new
tests walk all 75 bundled grammars and all seven bundled themes through the real
code that renders them.

### Assessment against the acceptance criteria

`EP120-REQ-1` — the maintained replacement is `bincode-next 3.1.2` in
`legacy()` configuration, recorded in `patches/syntect/PROVENANCE.txt` with the
archive checksum, upstream commit, licence, and the compatibility constraints
(endianness, fixed-int encoding, no fingerprint, trailing-byte tolerance).
Release notes searched: syntect 5.3.0 is still newest; upstream PR #694 is
open. `EP120-REQ-2` — the change is one manifest line, one new patch entry,
one vendored tree, one test, and generated bookkeeping; no unrelated upgrade is
included. `EP120-REQ-3` — met, with the evidence above. `EP120-REQ-4` — the
`diapason-ui` syntax-highlighting tests pass 18/18, `cargo check --locked
-p diapason-ui --all-targets` with `RUSTFLAGS="-D warnings"` exits 0, and
`cargo clippy --locked -p diapason-ui --all-targets -- -D warnings` exits 0.
`EP120-REQ-5` — behaviour and locked builds are covered above; the supported UI
paths are unchanged because no call site changed.

### Lessons

- The two advisories had different causes, and only one needed a code change.
  Checking *why* `yaml-rust` was in the graph first turned half the problem into
  a feature-flag fix. Starting from "we must fork syntect" would have vendored
  more than necessary.
- A vendored crate must not inherit the workspace's rustfmt settings, or its
  sources stop being comparable with upstream. `patches/syntect/rustfmt.toml`
  pins upstream's defaults, and the vendored tree is now byte-identical to the
  published archive except for `Cargo.toml` and `src/dumps.rs`.
- `cargo-unmaintained` conflates "I could not clone this" with "unmaintained".
  On a host whose `GIT_EXEC_PATH` is a credential shim, every new package looks
  unmaintained. Confirm cloneability independently before adding baseline
  entries.
- Comparing a compressed re-encode against an uncompressed asset produces a
  convincing-looking false failure. The probe had to compare like with like;
  that mistake is why the first probe run reported a 27 KB difference.

## Progress

- [x] 2026-11-24: read repository guidance, UI module guidance, and the sibling
  ExecPlans; loaded the codegraph, rust-router, hexagonal-architecture, and
  firecrawl skills. The `codegraph` MCP server timed out on connect, so
  reconnaissance used repository search tools instead.
- [x] 2026-11-24: captured the audit baseline with `cargo-audit-audit 0.22.2`
  against advisory database revision 1566 advisories. The run reports 12
  allowed warnings, including `bincode 1.3.3` (RUSTSEC-2025-0141),
  `yaml-rust 0.4.5` (RUSTSEC-2024-0320), `chrono 0.4.45`
  (RUSTSEC-2026-0543), `humantime 2.4.0` (RUSTSEC-2026-0542), `paste 1.0.15`
  (RUSTSEC-2024-0436), `zbus_names`/`zvariant`/`zvariant_derive` 5.13.1, two
  `unsound` findings (`rustix 1.1.4`, `tokio 1.53.1`), and two yanked
  packages (`async-executor 1.14.0`, `clipboard-win 5.4.1`).
- [x] 2026-11-24: confirmed both inverse dependency trees are
  `diapason-ui -> syntect 5.3.0 -> {bincode 1.3.3, yaml-rust 0.4.5}` and
  nothing else.
- [x] 2026-11-24: checked the "MSRV Check (1.88)" CI job. It installs rustc
  1.88.0, but `rust-toolchain.toml` overrides it to 1.93.0, and the job log
  says so: `info: note that the toolchain '1.93.0-x86_64-unknown-linux-gnu' is
  currently in use (overridden by
  '/home/runner/work/vtcode/vtcode/rust-toolchain.toml')`. The job therefore
  builds with 1.93.0, matches the workspace's declared MSRV, and is not
  evidence about a 1.88 floor. It does not constrain `bincode-next`'s 1.90
  requirement (see Decision log).
- [x] 2026-11-24: confirmed no published `syntect` release removes either
  advisory. crates.io's newest stable release is 5.3.0 (2025-09-27).
  `cargo update --dry-run -p syntect` has nothing to move to. Upstream PR #694
  (`serde-wincode`) and the 6.0.0 milestone remain unreleased.
- [x] 2026-11-24: verified `bincode-next` 3.1.2: MIT OR Apache-2.0, rust-version
  1.90.0, repository `Apich-Organization/bincode`, released 2026-11-13, no
  RustSec advisory, and transitive dependencies limited to `pastey`,
  `rapidhash`, and `unty-next` with the `serde`+`std` feature set.
- [x] Milestone 2 applied: `diapason-ui` features narrowed; `patches/syntect/`
  vendored; root `[patch.crates-io]` entry added; single unlocked resolution
  produced the intended 22-insertion/37-deletion lockfile delta.
- [x] Milestone 3 verified: the wire-compatibility probe decoded and re-encoded
  all bundled assets byte-identically, highlighted all 75 grammars, loaded all
  7 themes, and rejected a truncated dump, both before and after the final
  reformat of the vendored tree. Transcript below.
- [x] 2026-11-24: `cargo check --locked -p diapason-ui --all-targets` with
  `RUSTFLAGS="-D warnings"` exits 0 with no diagnostics.
- [x] 2026-11-24: `cargo clippy --locked -p diapason-ui --all-targets --
  -D warnings` exits 0.
- [x] 2026-11-24: the vendored fork compiles warning-free as an ordinary path
  dependency under `RUSTFLAGS="-D warnings"` (standalone probe, exit 0, 20m46s
  in the shared compile queue). No `#[allow(...)]` was added.
- [x] 2026-11-24: `cargo nextest run --locked -p diapason-ui -E
  'test(syntax_highlight)'` passes 18/18, including the two new tests.
- [x] 2026-11-24: `THIRD-PARTY-NOTICES` regenerated with cargo-about 0.9.2 and
  `scripts/generate-notices.sh --check` reports it up to date. `bincode` and
  `yaml-rust` are gone, `bincode-next`, `rapidhash`, and `unty-next` are added,
  and syntect is retained. `about.toml` needed no change: every new crate is
  MIT or Apache-2.0, both already accepted.
- [x] 2026-11-24: scanner baseline reconciled — the resolved `bincode` entry
  and the resolved `bincode` row under `aligned-vec` removed; no entry added.
- [x] 2026-11-24: `cargo audit --file Cargo.lock` no longer reports either
  target advisory and reports 10 allowed warnings, down from 12.
- [x] 2026-11-24: documentation updated in `docs/modules/diapason_ui.md` and
  `docs/execplans/ci-prerequisites.md`.
- [x] 2026-11-25: workspace-wide locked gates complete, all exit 0 —
  `cargo check --locked --workspace --all-targets --all-features` with
  `RUSTFLAGS="-D warnings"` (37m53s), `cargo clippy --locked --workspace
  --all-targets --all-features -- -D warnings` (12m05s), `cargo nextest run
  -p diapason-ui` 85/85, `-p diapason-core` 21/21, and the `diapason` binary
  171/171. `cargo audit --file Cargo.lock` against 1567 advisories exits 0 with
  10 allowed warnings and neither target advisory.

### Gate evidence

```plaintext
cargo check --locked --workspace --all-targets --all-features   EXIT_STATUS=0  DURATION=2275s
cargo clippy --locked --workspace --all-targets --all-features  EXIT_STATUS=0  DURATION=726s
cargo nextest run --locked -p diapason-ui      85 passed, 902 skipped   EXIT_STATUS=0
cargo nextest run --locked -p diapason-core    21 passed, 3763 skipped  EXIT_STATUS=0
cargo nextest run --locked -p diapason (bin)  171 passed, 2363 skipped  EXIT_STATUS=0
cargo audit --file Cargo.lock   10 allowed warnings, neither target ID   EXIT_STATUS=0
```

The workspace check was retried once by the gate runner after the first
attempt reached the runner's own time limit; the retry log is the
`.attempt-1-partial.out` sibling and the completed run is the one quoted. No
compiler diagnostic appeared in either attempt.

### Wire-compatibility transcript

The probe lives outside the repository at `/tmp/issue120-probe` and depends on
`patches/syntect` by path plus stock `bincode 1.3.3` for comparison. Rustc used
is the repository toolchain, 1.93.0.

```plaintext
default_newlines.packdump: 75 syntaxes, 368467 bytes
default_nonewlines.packdump: 75 syntaxes, 368082 bytes
highlighted 75 bundled grammars
themes: 7 loaded
82 checks executed
WIRE COMPATIBILITY: PASS
```

The probe prints the theme names on one line; that line is shortened to
`7 loaded` above to respect the prose line limit. The seven names, in the
probe's order, are `InspiredGitHub`, `Solarized (dark)`,
`Solarized (light)`, `base16-eighties.dark`, `base16-mocha.dark`,
`base16-ocean.dark`, and `base16-ocean.light`.

The probe asserts, per asset, that stock bincode 1.3.3 re-encodes the decoded
value to the original bytes *and* that the vendored codec re-encodes the same
value to the original bytes, in both syntect dump formats (raw and
zlib-compressed). A primitive-encoding matrix (unit, bool, `u8`, `u64`, `i64`,
`String`, `Vec<u8>`, `Vec<u32>`, `Option`, tuples, `HashMap`) also matched
byte-for-byte. Truncating one byte off a dump makes it fail to decode, so the
round-trip assertions are not vacuous.

## Surprises & discoveries

- The `yaml-rust` path is not a codec problem at all. It exists purely because
  `default-fancy` bundles `yaml-load`, and nothing in Diapason calls the YAML
  loader. Narrowing features removes it with no source change.
- Upstream fix in flight: PR #694 replaces bincode with `serde-wincode` and
  claims byte compatibility with bincode 1.3, and a commenter reports using the
  branch successfully with `regex-fancy` and the bundled dumps. It is
  unmerged, so the plan still vendors.
- `bincode-next`'s `legacy()` configuration is documented as "the default
  config that was present in bincode 1.0: little endian, fixed int length
  encoding". The remaining legacy behaviours (fingerprint disabled, bit packing
  off) are named directly in the configuration type.
- syntect's nested lazy contexts are dumped with the *compressed* helper
  (`dump_binary`), so the fork's codec must be correct for zlib-wrapped data as
  well as bare data.
- `cargo-unmaintained` reports every package whose repository it cannot clone
  as `Uncloneable`. On this host, `GIT_EXEC_PATH` pointed at the Lody GitHub
  credential shim, which returned HTTP 403 for all three new crates, so the
  first scan wrongly flagged `bincode-next`, `rapidhash`, and `unty-next`.
  Re-running with the system `GIT_EXEC_PATH` and no Lody credential helper
  found only the one genuine baseline change. This is a local-environment
  artefact, not a finding, and no baseline entries were added for it.
- The baseline entry for the `aligned-vec` crate carried a `bincode` row in its
  stale-dependency list. Removing bincode from the graph removed that row, so
  the entry had to be reconciled. This is the resolved-finding removal the plan
  allows, not a new entry.
- The scanner also flagged a `chrono`-adjacent HTTP 429 from crates.io during
  one run. That is transient rate limiting, not a finding; the check is retried
  rather than treated as a failure.

## Decision log

- Decision: narrow syntect features rather than patch away `yaml-load`.
  Rationale: Diapason uses none of the YAML, plist, or HTML loaders; removing
  unused features is smaller and removes an advisory at the same time.
- Decision: vendor via `[patch.crates-io]` rather than depend on a fork
  repository. Rationale: the repository already vendors `simdutf` this way, so
  the pattern, review surface, and supply-chain posture are established.
- Decision: `bincode-next` rather than `serde-wincode`, `wincode`,
  `bincode2`, `postcard`, or `bitcode`.
  - `bincode2` is a 2020-era fork of bincode 2 with no legacy-format mode.
  - `postcard`, `bitcode`, `wincode`, and `rkyv` are different formats and
    would require regenerating the bundled assets — explicitly out of bounds.
  - `serde-wincode` matches what upstream PR #694 uses, but the *crate* is a
    thin, single-maintainer layer; `bincode-next` is the continuation of the
    bincode project itself with an explicit, documented bincode-1 legacy mode.
    Either would work; `bincode-next` states the format contract in its own
    API rather than in a compatibility layer.
- Decision: pin `bincode-next` to `=3.1.2` rather than to `3.1` or to the
  newest `3.2.0`. Rationale: the vendored manifest must not drift with a
  registry update, and 3.2.0 landed two days before this change and rewrote
  the decoder internals. 3.1.2 is the newest patch on the line the wire
  compatibility was actually proven against.
- Decision: expose `syntect::dumps::DumpError` rather than keep
  `bincode::Result`. Rationale: `bincode`'s error type was the last bincode
  type in syntect's public surface, and keeping it would have leaked the
  `bincode-next` error type instead. The error is `#[non_exhaustive]` and
  implements `std::error::Error` with a source, so downstream code can still
  inspect it.
- Decision: drop `bincode-next`'s default features and enable only `serde` and
  `std`. Rationale: the default set also enables `derive` and `simd`, which
  need a proc-macro dependency syntect does not use. Narrower is cheaper to
  build and smaller to audit.
- Decision: give the vendored tree its own `rustfmt.toml`. Rationale: the
  workspace formatter is configured for 120-column lines; upstream syntect is
  formatted with rustfmt defaults. Without the local config, `cargo fmt` inside
  the fork would rewrite every file and destroy comparability with upstream.
- Rejected: switching `regex-fancy` to `regex-onig`. Rationale: unrelated to
  the advisories, and `onig` adds a C build dependency.
- Decision: do not fix the upstream syntect defects CodeRabbit reports against
  the vendored tree; record them here instead. Rationale: they are pre-existing
  upstream behaviour in files this change did not touch, and the vendored tree
  is meant to be byte-comparable with the published archive apart from the
  codec. Screening them:

  - `html` (9 findings), `yaml_load` (7), and `metadata` (3) are not compiled
    at all in this build: `src/lib.rs` gates `html` on `feature = "html"`,
    `src/parsing/mod.rs` gates `yaml_load` on `yaml-load` and `metadata` on
    `metadata`, and none of those features is enabled. A defect there cannot
    affect the shipped binary.
  - The `util.rs` LaTeX findings concern `as_latex_escaped`, which no Diapason
    code calls.
  - The remainder are reachable only by feeding syntect a hand-crafted or
    malformed grammar. Diapason highlights with the grammars bundled inside
    syntect's own binary, which are fixed at build time, and never loads a
    grammar or theme from a file or from user input. The one path that does
    parse untrusted text is `highlight_line` over agent output, and the
    reported defects there are behavioural edges (a non-advancing transition, a
    missing main context) on a grammar Diapason itself supplies, not memory
    unsafety.
  - None is a memory-safety or soundness issue, so none interacts with the
    codec change this plan is accountable for.

  Fixing them in the vendored copy would silently fork syntect's behaviour and
  make the next upstream sync a merge exercise. They belong upstream, and the
  exit path already routes them there. The one finding in the vendored tree
  that was about *this* change — that the four assets were missing — was wrong,
  and was reported as `critical` in both review passes. Checked four ways:
  `git rev-parse HEAD:patches/syntect/assets/<file>` resolves a blob for each,
  each `HEAD` blob hashes equal to its worktree file, each file is byte-equal to
  the crates.io archive, and all four are present in the pushed `origin` branch.
  The crate also compiles with `dump-load`, `default-syntaxes`, and
  `default-themes` enabled and the tests decode all four, which is only possible
  if the paths resolve. Nothing to fix; do not "restore" the assets.

  Reassess if Diapason ever starts loading grammars or themes from files, which
  would bring `yaml_load` into the build and make its findings live.

## Revision note

2026-11-24: first revision after implementation. Added the wire-compatibility
and UI-test transcripts, the completed `Outcomes & retrospective` with its
per-criterion assessment, the four new decision-log entries (version pin, error
type, feature set, vendored rustfmt config), the resolved scanner-baseline
details, and the falsified assumptions recorded under
`Surprises & discoveries`. The `Verification plan` now records discharge
evidence per invariant rather than the planned method alone. Remaining work is
the workspace-wide gate run and the pull request.
