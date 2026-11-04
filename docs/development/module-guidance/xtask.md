# xtask

> Retained module guidance, originally written for VTCode and updated to
> Diapason naming. Current path policy supersedes older details: user config is
> XDG `config.toml` plus `AGENTS.md`, durable logs/audit/sessions use XDG
> state, workspace data defaults to configurable `.diapason/`, and legacy
> `.vtcode/` paths are never read.

[Root AGENTS.md](../../../AGENTS.md) | Release packaging automation for
`cargo-binstall` extra-files layout.

## Role

Generates release archives with man pages and shell completions in the
directory layout that `cargo-binstall` auto-detects.

## Commands

```
cargo xtask package-release --target <triple> --version <ver> --binary <path>
```

## Rules

- Runs on the host (not cross-compiled) -- only generates text files and copies
  binaries.
- Archive layout: `diapason-{target}-v{version}/` with `man/`, `completions/`.
- The archive stage allowlist is intentional: package only `diapason`, `man/`,
  and `completions/`; project instruction files never ship.
- Uses `diapason_core::Cli::command()` for man page and completion generation.
