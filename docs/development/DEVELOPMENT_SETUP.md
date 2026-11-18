# Development Setup

Canonical local setup for contributing to Diapason.

## Prerequisites

- Rust toolchain (stable) via [rustup](https://rustup.rs/)
- Git
- An LLM provider credential: either (a) a shell/workspace env var like
  `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`,
  `ZAI_API_KEY`, `MOONSHOT_API_KEY`, `STEPFUN_API_KEY`, or `MINIMAX_API_KEY`,
  (b) an OAuth session for an auth-managed provider, or (c) a key stored via
  `diapason secret add <provider>`.

## One-Time Setup

```bash
git clone https://github.com/leynos/vtcode.git diapason
cd diapason
./scripts/setup.sh --with-hooks
```

`./scripts/setup.sh` verifies `rustfmt`/`clippy`, installs `cargo-nextest` when
missing, and runs `cargo check`.

## Credential identities

Secure API-key storage is scoped by `(provider, key name)`, where the key name
is the environment variable used for that credential. This keeps multiple
profiles for one provider independent:

```bash
diapason secret add mimo --key-name MIMO_API_KEY
diapason secret add mimo --key-name MIMO_TOKEN_PLAN_KEY
diapason secret status mimo --key-name MIMO_TOKEN_PLAN_KEY
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

This runs formatting checks, linting, governance checks, build, tests
(nextest-first), and docs generation.

## Makefile Gate

The repository-root `Makefile` provides the local release/PR checks as
individual targets. `make` uses `check` as its default target. The complete
gate runs sequentially in this order: `check-fmt`, `lint`, `build`, `test`,
`test-harness`, `check-ast-grep`, and `advisory`. The `typecheck` target is
available separately and is not part of the default `check` dependency list.

These are the public targets:

| Target           | Runs                                                     |
| ---------------- | -------------------------------------------------------- |
| `check`          | The complete sequential gate                             |
| `check-fmt`      | `cargo fmt --all -- --check`                             |
| `lint`           | Shell, policy, Clippy and documentation checks           |
| `lint-shell`     | Shell syntax and truncated-command checks                |
| `lint-policies`  | Workflow-security and structured-logging checks          |
| `lint-clippy`    | Workspace Clippy with warnings denied                    |
| `lint-docs`      | Workspace documentation generation without dependencies  |
| `build`          | Locked workspace build                                   |
| `typecheck`      | Locked workspace check with all targets and features     |
| `test`           | Locked workspace Nextest run                             |
| `test-harness`   | PTY, pipe and inline-event harness suites                |
| `check-ast-grep` | The Diapason ast-grep check when `ast-grep` is installed |
| `advisory`       | Warn-mode source hygiene and legibility reports          |

The gate accepts these variables for local toolchain and test-runner overrides:

- `CARGO` selects the Cargo executable (default: `cargo`).
- `BUILD_JOBS` passes the Cargo job setting (default: `--jobs 6`).
- `NEXTEST_PROFILE` selects the Nextest profile (default: `default`).

The gate requires `make`, Rust tooling (`cargo fmt`, Clippy and Cargo
documentation), Python 3 for advisory checks, Bash for shell checks, and
`cargo-nextest` for tests. The ast-grep scan is optional: `check-ast-grep`
reports a skip when the `ast-grep` executable is unavailable. See
[Structural Rule Checks](testing.md#structural-rule-checks) for installation
and workspace setup instructions.

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

## Configuration field reference

`docs/config/CONFIG_FIELD_REFERENCE.md` is generated from the `diapason-config`
schema. Regenerate it, then reformat the result:

```bash
python3 scripts/generate_config_field_reference.py
make fmt
```

`--output <path>` writes somewhere other than the committed reference.
`--schema <snapshot>` reads a previously captured schema JSON instead of
invoking Cargo, and `--home-dir <path>` normalises absolute defaults against
the snapshot host's home, for reviewing a change without running the schema
example locally.

The schema walk emits a row per leaf and per union branch, never for an object
that has properties. `REFERENCE_SUPPLEMENT_ROWS` therefore supplies the
`custom_providers[].pricing` and `custom_providers[].profiles.*.pricing` parent
rows, and `scripts/tests/test_generate_config_field_reference.py` pins them and
their no-duplicate behaviour. Regeneration can still add rows the committed
table does not list yet, such as most `custom_providers[].rate_limit_headers`
children, so review the custom-provider rows as a diff before committing, and
never delete a supplement to quiet one.

## Troubleshooting

- `cargo nextest` missing:
  - Run `cargo install cargo-nextest --locked`
- No provider credential found:
  - Run `diapason secret add <provider>` to store a key in your OS keyring
    (recommended), or
  - Use `diapason secret add <provider> --key-name <ENV_VAR>` for a non-default
    provider profile, or
  - `export OPENAI_API_KEY="sk-..."` (or the equivalent env var for your
    provider) in your shell, or
  - Run `diapason login <provider>` for OAuth/managed-auth providers (copilot,
    openai, openrouter).
- Script permissions:
  - Run `chmod +x scripts/*.sh`
