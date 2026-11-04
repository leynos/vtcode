# diapason-macros

> Retained module guidance, originally written for VTCode and updated to
> Diapason naming. Current path policy supersedes older details: user config is
> XDG `config.toml` plus `AGENTS.md`, durable logs/audit/sessions use XDG
> state, workspace data defaults to configurable `.diapason/`, and legacy
> `.vtcode/` paths are never read.

Procedural macros for Diapason. Contains derive macros and attribute macros
shared across workspace crates.

## Conventions

- This is a proc-macro crate. It must not depend on any other diapason workspace
  crate.
- Use `syn` for parsing, `quote` for code generation, `proc-macro2` for token
  streams.
- Keep macro implementations minimal -- generate code that delegates to runtime
  helpers in other crates.
- All macros must have doc comments with usage examples.

## Dependencies

- `syn` (parsing)
- `quote` (code generation)
- `proc-macro2` (token streams)
