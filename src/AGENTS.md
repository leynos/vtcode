# Diapason binary guidance

[Root guidance](../AGENTS.md) applies. This crate owns CLI wiring and process
bootstrap.

- Keep startup and CLI responsibilities in `src/`; reusable logic belongs in
  workspace crates.
- Resolve global paths through `DiapasonPaths`; use the shared
  workspace-directory helper for repository-local data.
- Never scan legacy `.vtcode/` paths. Global config is `config.toml` with
  `AGENTS.md` under XDG config; sessions, audit and logs use XDG state.
- Treat command execution and dynamic workspace content as untrusted. Preserve
  sandbox policy, approval and fail-closed behaviour.
- Use `ThreadEvent` as the runtime event contract. See the crate source and
  [development setup](../docs/development/DEVELOPMENT_SETUP.md). Detailed
  module guidance: [src](../docs/development/module-guidance/src.md).
