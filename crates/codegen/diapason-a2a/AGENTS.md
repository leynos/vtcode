# diapason-a2a guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep protocol transport, serialization and task lifecycle aligned with the
  A2A specification.
- Validate remote inputs and URLs before network or filesystem effects.
- Preserve bounded payload handling and explicit error outcomes.
Detailed module guidance:
[diapason-a2a](../../../docs/development/module-guidance/diapason-a2a.md).
