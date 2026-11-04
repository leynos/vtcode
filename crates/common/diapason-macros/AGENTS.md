# diapason-macros guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep procedural macros small, deterministic and diagnostic-rich.
- Do not add runtime dependencies or silently change generated public APIs.
- Cover emitted code and compile-failure behavior with focused tests.
Detailed module guidance:
[diapason-macros](../../../docs/development/module-guidance/diapason-macros.md).
