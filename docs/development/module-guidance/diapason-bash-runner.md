# diapason-bash-runner

> Retained module guidance, originally written for VTCode and updated to
> Diapason naming. Current path policy supersedes older details: user config is
> XDG `config.toml` plus `AGENTS.md`, durable logs/audit/sessions use XDG
> state, workspace data defaults to configurable `.diapason/`, and legacy
> `.vtcode/` paths are never read.

[Root AGENTS.md](../../../AGENTS.md) | Cross-platform command runner with
workspace-safe operations.

## Modules

`executor` CommandExecutor trait + backends | `runner` BashRunner | `policy`
CommandPolicy + WorkspaceGuardPolicy | `pipe` async process spawning |
`process` handles | `process_group` kill/cleanup | `background` long-running
tasks | `stream` utilities

## Rules

- `CommandExecutor` trait = primary abstraction for new backends.
- `CommandPolicy` trait = execution gate. `WorkspaceGuardPolicy` enforces
  boundaries.
- Feature flags: `dry-run`, `pure-rust`, `exec-events`, `serde-errors`.
- `process_group` uses safe `nix` wrappers (`Pid::from_raw`, `signal::killpg`,
  `setpgid` in `pre_exec`); there is no `unsafe` here. Unsafe env mutation is
  centralized in `diapason-commons::env_lock`, serialized by a process-wide
  mutex.

## Testing

`cargo nextest run -p diapason-bash-runner` | pipe tests:
`cargo nextest run -p diapason-bash-runner -E 'binary(/pipe_tests/)'` | use
`AllowAllPolicy` unless testing policy.

## Gotchas

- `BashRunner::new()` canonicalizes root — bails if missing.
- `path_cache` is LRU (256) — no fresh canonicalize every call.
- Unsafe env mutation (`set_var`/`remove_var`) is centralized in
  `diapason-commons::env_lock`, serialized by a process-wide mutex,
  single-threaded startup only.
- `policy` containment delegates to
  `diapason_commons::paths::ensure_path_within_workspace` — `..`-traversal
  paths are rejected (intentionally stricter than the old `starts_with`).
- Pipe spooling opts into `SpawnedProcess::reliable_output_rx`, a bounded
  lossless stream; legacy broadcast subscribers must remain independent of that
  backpressure path.
- `wait_with_output` bounds post-exit draining even when a descendant inherits
  the pipe; never turn that drain back into an unbounded wait.
