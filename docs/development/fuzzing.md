# Fuzzing Guide

This guide describes local fuzz testing for VT Code with `cargo-fuzz`.

## Scope

Current fuzz targets focus on security parser surfaces in `vtcode-core`:

- `shell_parser`: `command_safety::shell_parser` parsing paths
- `exec_policy_parser`: `exec_policy::PolicyParser` (simple/TOML/JSON)
- `exec_policy_command_validation`:
  `exec_policy::command_validation::validate_command`
- `unified_path_validation`:
  `tools::validation::unified_path::validate_and_resolve_path`

## Prerequisites

VT Code defaults to stable Rust. Fuzzing uses nightly explicitly.

```bash
# Install cargo-fuzz once
cargo install cargo-fuzz --locked

# Install nightly toolchain (keeps stable as default)
rustup toolchain install nightly
```

## Basic Commands

Run from repository root:

```bash
# List available fuzz targets
cargo +nightly fuzz list

# Build a target
cargo +nightly fuzz build shell_parser

# Run for 60 seconds
cargo +nightly fuzz run shell_parser -- -max_total_time=60
```

Other targets:

```bash
cargo +nightly fuzz run exec_policy_parser -- -max_total_time=60
cargo +nightly fuzz run exec_policy_command_validation -- -max_total_time=60
cargo +nightly fuzz run unified_path_validation -- -max_total_time=60
```

## Corpus and Artifacts

- Seed corpus: `fuzz/corpus/<target>/`
- Crash artifacts: `fuzz/artifacts/<target>/`
- Coverage outputs: `fuzz/coverage/<target>/`

## Reproducing a Crash

Given an artifact like `fuzz/artifacts/shell_parser/crash-...`:

```bash
cargo +nightly fuzz run shell_parser fuzz/artifacts/shell_parser/crash-...
```

## Replaying a Coverage proptest failure

The Coverage workflow records one random decimal `PROPTEST_RNG_SEED` in the job
summary before it runs the tests. Read that value from the failed job's summary
and replay the property with it. Leave `PROPTEST_CASES` unset so the replay uses
the same case count as the hosted run.

```bash
export PROPTEST_RNG_SEED=<seed-from-the-job-summary>

# Coverage arm, matching the hosted job's command.
cargo llvm-cov nextest --locked --workspace --lcov --output-path lcov.info

# Ordinary arm, default and CI profiles.
cargo nextest run --locked -p vtcode-core --test core_fuzz_props \
  -E 'test(fuzz_shell_parser_never_panics)'
cargo nextest run --locked -p vtcode-core --test core_fuzz_props \
  -E 'test(fuzz_shell_parser_never_panics)' --cargo-profile ci
```

The hosted job runs the whole workspace, while the filtered arms run one binary
in its own process. Nextest isolates each test in its own process, so parser
state cannot leak between tests; the difference that matters is test ordering
and the set of other binaries, which only the workspace-wide command
reproduces. Use the workspace command when the result must match the hosted run
exactly.

A native signal kills the process before proptest can shrink the input or write
a `.proptest-regressions` entry. To recover the script, run a scratch harness
that records each generated script and flushes it before calling
`parse_shell_commands`. Generated scripts may contain newlines, so a line per
script is ambiguous; write one JSON string per record instead, which escapes
newlines and keeps each script on one line. The scratch harness needs
`serde_json` as a dev-dependency:

```rust
writeln!(log, "{}", serde_json::to_string(&script)?)?;
log.flush()?;
let _parsed = parse_shell_commands(&script);
```

The last record is the crashing input; `serde_json::from_str::<String>` on that
line recovers the exact bytes. Keep the preceding records as well, because the
crash may depend on parser state accumulated by earlier cases.

## Coverage (Optional)

```bash
cargo +nightly fuzz coverage shell_parser
```

Then inspect `fuzz/coverage/shell_parser/coverage.profdata` with your preferred
LLVM coverage tooling.
