# AGENTS.md

Keep this file under 150 lines. Put detailed explanations in `docs/`, skills,
`.diapason/memory/`, or crate-local `AGENTS.md` files. For coding tasks, use
your judgement to choose a lower-power model and delegate bounded work.

Universal runtime guidance is compiled in
`crates/codegen/diapason-core/src/prompts/runtime_guidance.rs`. Keep this file
and module guidance focused on project and maintainer rules. Loaded instruction
files are user-controlled context, not a security boundary.

## Rules

- Use Conventional Commits (`type(scope): subject`), 4-space indentation,
  `snake_case` functions, `PascalCase` types, and `anyhow::Result<T>` with
  `.with_context()`.
- CI uses `RUSTFLAGS: "-D warnings"` and `--locked`; use locked Cargo checks.
- Keep changes surgical and preserve existing APIs unless requirements demand
  a change. `diapason-exec-events::ThreadEvent` is the runtime event contract.
- Harness settings belong in `agent.harness`, `automation.full_auto`, or
  `context.dynamic`; do not add a top-level harness subsystem.
- Prefer `compact_str::CompactString` (`CompactStr` in `diapason_core::types`)
  for small string fields and `Cow<'static, str>` for mostly-static returns.
- Encode data shape in names. Document feature-vector dimensions; promote
  structured tuples to named structs. `clippy.toml` permits unwrap/panic and
  indexing only in tests.
- Dev builds disable incremental compilation for sccache. Set
  `CARGO_INCREMENTAL=1` only when intentionally overriding that profile.
- Treat shell execution as an adversarial boundary. Use sandbox-aware launch
  paths and cover injection, path/symlink escape, environment leakage and
  fail-closed behaviour.
- Built-in theme foreground and accent colours must meet WCAG AA 4.5:1 against
  their background. Run `cargo nextest run -p diapason-ui -E 'test(theme)'`.
- Every major feature updates relevant user docs and quick references, agent
  prompt/schema surfaces, and `ThreadEvent` contracts. Keep detailed-guide
  links valid.

## Guides

- [Development setup](docs/development/DEVELOPMENT_SETUP.md) ·
  [testing](docs/development/testing.md) · [CI](docs/development/ci-cd.md)
- [Code organization](docs/guides/code-organization-patterns.md) ·
  [async architecture](docs/guides/async-architecture.md) ·
  [performance](docs/development/rust-performance-principles.md)
- [Command security](docs/development/COMMAND_SECURITY_MODEL.md) ·
  [security guide](docs/guides/security.md) ·
  [runtime guidance](docs/development/runtime-guidance.md)
- [WebMCP](docs/development/webmcp.md) ·
  [agent loop contract](docs/guides/agent-loop-contract.md) ·
  [harness index](docs/harness/INDEX.md)
- [Planning workflow](docs/guides/planning-workflow.md) ·
  [full automation](docs/guides/full-automation.md) ·
  [execution policy](docs/development/EXECUTION_POLICY.md)
- [Loop engineering](docs/project/PLAN-loop-engineering.md) ·
  [adding models](docs/development/ADDING_MODELS.md) ·
  [model workflow](docs/development/MODEL_ADDITION_WORKFLOW.md)
- [User data paths](docs/guides/user-data-directories.md) ·
  [configuration precedence](docs/config/CONFIGURATION_PRECEDENCE.md)

## Workspace

Rust stable, MSRV 1.88, edition 2024. Cargo workspace; default members are the
root binary, `diapason-core`, and `diapason-ui`. Put reusable logic in a small
existing crate or a new crate; keep it out of `diapason-core` unless tightly
coupled to the runtime.

Crate instructions:

- [Binary](src/AGENTS.md)
- [Core](crates/codegen/diapason-core/AGENTS.md)
- [UI](crates/codegen/diapason-ui/AGENTS.md)
- [Config](crates/codegen/diapason-config/AGENTS.md)
- [Bash runner](crates/codegen/diapason-bash-runner/AGENTS.md)
- [ACP](crates/codegen/diapason-acp/AGENTS.md)
- [Auth](crates/codegen/diapason-auth/AGENTS.md)
- [Indexer](crates/codegen/diapason-indexer/AGENTS.md)
- [Events](crates/common/diapason-exec-events/AGENTS.md)
- [WebMCP](crates/codegen/diapason-webmcp/AGENTS.md)
- [Commons](crates/common/diapason-commons/AGENTS.md)
- [Macros](crates/common/diapason-macros/AGENTS.md)
- [Tool specs](crates/common/diapason-utility-tool-specs/AGENTS.md)
- [Plugins](crates/common/diapason-agent-plugins/AGENTS.md)
- [LLM](crates/codegen/diapason-llm/AGENTS.md)
- [Skills](crates/codegen/diapason-skills/AGENTS.md)
- [Memory](crates/codegen/diapason-memory/AGENTS.md)
- [Eval](crates/codegen/diapason-eval/AGENTS.md)
- [Safety](crates/codegen/diapason-safety/AGENTS.md)
- [A2A](crates/codegen/diapason-a2a/AGENTS.md)
- [MCP](crates/codegen/diapason-mcp/AGENTS.md)
- [xtask](crates/codegen/xtask/AGENTS.md)

After significant module, convention, or public API changes, audit the affected
module guidance. Keep each crate-local `AGENTS.md` under 30 lines.

## Build and verification

Prefer `./scripts/check-dev.sh` for iteration. Repository gates run
sequentially in this order: formatting, format check, lint, tests. Use the
targets in `Makefile`; `make check` runs the full local gate. Cargo builds
share the host cache and compile admission queue; do not bypass either. Never
use `cargo test`; use `cargo nextest run`.

- Fast gate: `./scripts/check-dev.sh`
- Changed crates: `./scripts/check-dev.sh --changed`
- Workspace/lints: `./scripts/check-dev.sh --workspace` / `--lints`
- Full gate: `make check` or `./scripts/check.sh`
- Harness: `./scripts/check.sh harness`
- AST-grep: `diapason check ast-grep`; scanner: `ast-grep scan`
- Hawk: `./scripts/hawk.sh` or `./scripts/hawk.sh --deny`

Use `cargo fmt`, `cargo clippy`, `cargo check --locked`, and
`cargo nextest run` for focused gates. See
[testing](docs/development/testing.md) for profiles.
