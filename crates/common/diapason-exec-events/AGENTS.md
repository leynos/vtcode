# diapason-exec-events guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- `ThreadEvent` is the authoritative runtime event contract; do not create
  parallel event types.
- Preserve schema versioning, stable serialization and compatibility handling.
- Keep ATIF and other exports derived from the canonical event stream.
Detailed module guidance:
[diapason-exec-events](../../../docs/development/module-guidance/diapason-exec-events.md).
