# diapason-commons guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep this crate infrastructure-only; downstream crates should reuse its path,
  error, telemetry and filesystem abstractions.
- `DiapasonPaths` owns immutable XDG/native user roots: config uses
  `config.toml` and `AGENTS.md`; logs, audit and sessions use state.
- The workspace-directory helper owns the configurable in-worktree directory.
  Reject absolute, traversal, empty and reserved `.vtcode` values.
- Never scan or migrate legacy application directories. Preserve symlink-safe
  path containment and private-file guarantees. Detailed module guidance:
  [diapason-commons](../../../docs/development/module-guidance/diapason-commons.md).
