# AGENTS.md

Keep this file concise and under 150 lines. Root guidance belongs here;
detailed explanations belong in `docs/`, skills, `.vtcode/memory/`, or
crate-local `AGENTS.md` files. For all coding tasks use your judgement to
decide an appropriate lower power model and run that in a subagent.

Universal model-facing behavior is compiled in
`crates/codegen/vtcode-core/src/prompts/runtime_guidance.rs`. Keep this file
and module `AGENTS.md` files focused on project and maintainer guidance;
dynamically loaded instruction files are user-controlled context, not a
security boundary.

## Rules

- `origin` (`leynos/vtcode`) is the canonical hard fork. `upstream` is
  reference-only; do not merge its commits or target it with pull requests.
- Conventional Commits (`type(scope): subject`).
- 4-space indentation, `snake_case` fns, `PascalCase` types,
  `anyhow::Result<T>` + `.with_context()`.
- CI sets `RUSTFLAGS: "-D warnings"` and uses `--locked`. Match locally with
  `cargo check --locked` when relevant.
- Keep changes surgical. Preserve existing APIs unless the task requires a
  change.
- `vtcode-exec-events::ThreadEvent` is the authoritative runtime event contract
  — do not invent parallel types.
- Harness config is split across `agent.harness`, `automation.full_auto`,
  `context.dynamic` — do not add a new top-level harness subsystem.
- Prefer `compact_str::CompactString` (aliased as `CompactStr` in
  `vtcode_core::types`) over `String` for small string fields. Use
  `Cow<'static, str>` for mostly-static return strings.
- **Shape-suffix naming**: encode the dimensional structure of data in
  variable/type names. For feature vectors, document a **dimension key** (table
  of index → name → meaning). For bare tuples holding structured data, promote
  to named structs so the shape is explicit in the type system (inspired by
  Noam Shazeer's shape-suffix convention).
- `clippy.toml` allows `unwrap`/`panic`/indexing in tests only.
- Dev profile has `incremental = false` (sccache). Set `CARGO_INCREMENTAL=1` to
  override.
- Treat the sandbox/exec boundary as a primary adversarial surface: use
  sandbox-aware launch paths and add adversarial regression coverage for
  command injection, path/symlink escape, environment leakage, and fail-closed
  behavior.
- **All built-in themes must meet WCAG AA 4.5:1 contrast** for foreground and
  all accent fields against background. Validate with
  `cargo nextest run -p vtcode-ui -E 'test(theme)'`. See
  `.vtcode/memory/gotchas.md` for catppuccin-latte special-case.
- **Every new major feature must update docs**: user-facing behavior →
  `docs/development/` guide + a table row/section in the relevant
  quick-reference; agent-facing tool surface → prompt guidance
  (`crates/codegen/vtcode-core/src/prompts/guidelines.rs`) + schema
  (`crates/common/vtcode-utility-tool-specs`); runtime contract →
  `vtcode-exec-events::ThreadEvent`. No feature is "done" until its docs are
  updated and links in the required workflow reference still resolve.

## Workspace and modules

Cargo workspace, ~30 crates; Rust stable, MSRV 1.93.0, edition 2024.
`default-members` are root, `vtcode-core`, and `vtcode-ui`. Put reusable logic
in a small existing or new crate; keep it out of `vtcode-core` unless tightly
coupled to the core runtime.

Every crate has a local `AGENTS.md` with crate-specific conventions. After
significant module, convention, gotcha, or public API changes, use the
`audit-module-agents` skill to check whether it needs updating. Keep local
`AGENTS.md` files under 30 lines.

The required
[agent workflow reference](docs/development/agent-workflow-reference.md)
contains workspace and per-module catalogues, detailed guide links, and build
and test commands. Read it when a task touches those paths, crate conventions,
or verification gates.

## Project memory

Session-independent knowledge lives in the gitignored `.vtcode/memory/` files:
`gotchas.md`, `issues.md`, `library.md`, `decisions.md`, and `scratch.md`. Read
them when context is needed; write durable learnings there. See
`.vtcode/memory/README.md` for format rules.

## Testing rule

Always use `cargo nextest run`; **never use `cargo test`**. The workflow
reference documents test profiles, focused commands, and repository gates.

## Skills and special workflows

- Skills use the Skill tool; subagents use the Agent tool. Project slash
  commands are in `.claude/commands/`; agents are in `.claude/agents/`.
- For LLM providers, use `adding-llm-providers`. The `/model` picker uses
  `ModelId::all_models()`; `ModelsManager` uses `builtin_model_presets()`.
- For workspace crates, use `adding-workspace-crate`; workspace path
  dependencies need `version` fields.
- For structural code work, prefer `ast-grep` over text grep. Use `rg` for
  prose, logs, and config. Invoke `ast-grep`, not the `sg` alias; use
  `exec_command` or its skill for arbitrary patterns. Advanced `code_search`
  accepts one literal query and bounded filters.
- Cap large command output: `COMMAND 2>&1 | head -c 4000`.
