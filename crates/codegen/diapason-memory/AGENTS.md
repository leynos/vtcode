# diapason-memory guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[session persistence](../../../docs/development/session-persistence.md).

- `ThreadEvent` is the canonical append-only session log; derived views must
  remain reconstructible from it.
- Workspace-scoped event sessions live under
  `$XDG_STATE_HOME/diapason/workspaces/<workspace-hash>/sessions/`; global
  session archives use XDG state.
- Keep retention scoped to validated direct children, preserve active sessions,
  skip symlinks and fail closed on unsafe paths.
- Never read or migrate `.vtcode/` directories. Workspace agent files use the
  shared configurable `.diapason/` directory helper. Detailed module guidance:
  [diapason-memory](../../../docs/development/module-guidance/diapason-memory.md).
