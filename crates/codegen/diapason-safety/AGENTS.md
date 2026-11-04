# diapason-safety guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[security guide](../../../docs/guides/security.md).

- Own command safety detection, execution policies and sandbox decisions;
  callers must not fork these checks.
- Default to fail-closed at untrusted input boundaries. Preserve path and
  symlink containment.
- Cover injection and environment leakage with adversarial tests.
Detailed module guidance:
[diapason-safety](../../../docs/development/module-guidance/diapason-safety.md).
