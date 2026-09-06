# Development Setup

Canonical local setup for contributing to VT Code.

## Prerequisites

- Rust toolchain (stable) via [rustup](https://rustup.rs/)
- Git
- An LLM provider credential: either (a) a shell/workspace env var like `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `ZAI_API_KEY`, `MOONSHOT_API_KEY`, `STEPFUN_API_KEY`, or `MINIMAX_API_KEY`, (b) an OAuth session for an auth-managed provider, or (c) a key stored via `vtcode secret add <provider>`.

## One-Time Setup

```bash
git clone https://github.com/vinhnx/vtcode.git
cd vtcode
./scripts/setup.sh --with-hooks
```

`./scripts/setup.sh` verifies `rustfmt`/`clippy`, installs `cargo-nextest` when missing, and runs `cargo check`.

## Credential identities

Secure API-key storage is scoped by `(provider, key name)`, where the key name is
the environment variable used for that credential. This keeps multiple profiles
for one provider independent:

```bash
vtcode secret add mimo --key-name MIMO_API_KEY
vtcode secret add mimo --key-name MIMO_TOKEN_PLAN_KEY
vtcode secret status mimo --key-name MIMO_TOKEN_PLAN_KEY
```

Environment variables and workspace `.env` values take precedence over secure
storage. Existing provider-only entries are migrated lazily only when the
requested key is the provider default; non-default profiles require an explicit
key name. Configured `[[custom_providers]]` and `[providers.<name>]` overrides
use the same identity rules.

For debug or release launches:

```bash
./scripts/run.sh
./scripts/run-debug.sh
```

## Daily Development Loop

```bash
# Fast compile check
cargo check

# Fast test loop (recommended)
cargo nextest run

# Fallback if nextest is unavailable
cargo test --workspace
```

## Full Quality Gate

```bash
./scripts/check.sh
```

This runs formatting checks, linting, governance checks, build, tests (nextest-first), and docs generation.

## Makefile Gate

The repository-root `Makefile` provides the local release/PR checks as
individual targets. `make` uses `check` as its default target. The complete
gate runs sequentially in this order: `check-fmt`, `lint`, `build`, `test`,
`test-harness`, `check-ast-grep`, and `advisory`. The `typecheck` target is
available separately and is not part of the default `check` dependency list.

These are the public targets:

| Target | Runs |
| --- | --- |
| `check` | The complete sequential gate |
| `check-fmt` | `cargo fmt --all -- --check` |
| `lint` | Shell, policy, spelling, workflow, Clippy and documentation checks |
| `lint-shell` | Shell syntax and truncated-command checks |
| `lint-policies` | Workflow-security and structured-logging checks |
| `spelling` | Check existing Git-tracked files against `typos.toml` |
| `test-spelling` | Focused tracked-file and CI spelling contracts |
| `github-actions-lint` | Yamllint and actionlint workflow validation |
| `lint-clippy` | Workspace Clippy with warnings denied |
| `lint-docs` | Workspace documentation generation without dependencies |
| `build` | Locked workspace build |
| `typecheck` | Locked workspace check with all targets and features |
| `test` | Locked workspace Nextest run |
| `test-harness` | PTY, pipe and inline-event harness suites |
| `check-ast-grep` | The VT Code ast-grep check when `ast-grep` is installed |
| `advisory` | Warn-mode source hygiene and legibility reports |

The gate accepts these variables for local toolchain and test-runner
overrides:

- `CARGO` selects the Cargo executable (default: `cargo`).
- `BUILD_JOBS` passes the Cargo job setting (default: `--jobs 6`).
- `NEXTEST_PROFILE` selects the Nextest profile (default: `default`).

The gate requires `make`, Rust tooling (`cargo fmt`, Clippy and Cargo
documentation), Python 3 for advisory checks, Bash for shell checks,
typos-cli for spelling, yamllint and actionlint for workflows, and
`cargo-nextest` for tests. The ast-grep scan is optional:
`check-ast-grep` reports a skip when the `ast-grep` executable is unavailable.
See [Structural Rule Checks](testing.md#structural-rule-checks) for
installation and workspace setup instructions.

## Spelling Gate

Install the CI-aligned spelling tool locally, then run the focused gate:

```bash
cargo install typos-cli@1.50.1 --locked
make spelling
```

`make spelling` discovers files with `git ls-files -z`, checks only paths that
still exist, and skips deleted or empty sets. It passes the explicit
`typos.toml` policy with `--force-exclude`, so untracked and ignored files are
not added to the check. The policy keeps only documented exact external/API or
wire terms and fixed historical identifiers and hashes; correct ordinary prose
rather than adding an exception. `make lint` invokes this target. The spelling
tool is separate from the unchanged Rust toolchain configuration.

## Common Commands

```bash
# Format
cargo fmt --all

# Lint (deny warnings)
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Build
cargo build --workspace

# Docs
cargo doc --workspace --no-deps

# Docs, including private items (maintainer inspection)
cargo doc --workspace --no-deps --document-private-items
```

## Troubleshooting

- `cargo nextest` missing:
  - Run `cargo install cargo-nextest --locked`
- No provider credential found:
  - Run `vtcode secret add <provider>` to store a key in your OS keyring (recommended), or
  - Use `vtcode secret add <provider> --key-name <ENV_VAR>` for a non-default provider profile, or
  - `export OPENAI_API_KEY="sk-..."` (or the equivalent env var for your provider) in your shell, or
  - Run `vtcode login <provider>` for OAuth/managed-auth providers (copilot, openai, openrouter).
- Script permissions:
  - Run `chmod +x scripts/*.sh`
