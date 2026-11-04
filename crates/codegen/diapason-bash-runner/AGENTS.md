# diapason-bash-runner guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[command security model](../../../docs/development/COMMAND_SECURITY_MODEL.md).

- This crate owns sandbox-aware shell execution. Preserve fail-closed policy
  and explicit approval boundaries.
- Cover command injection, path/symlink escape and environment leakage with
  adversarial regressions.
- Never bypass the configured execution policy or invoke a shell through an
  unsafe fallback. Detailed module guidance:
  [diapason-bash-runner](../../../docs/development/module-guidance/diapason-bash-runner.md).
