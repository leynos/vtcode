# diapason-eval guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep evaluation outcomes deterministic and distinguish environment failure
  from agent behaviour.
- Preserve pass@k/pass^k definitions and regression baselines.
- Do not treat an incomplete or skipped evaluation as a pass.
Detailed module guidance:
[diapason-eval](../../../docs/development/module-guidance/diapason-eval.md).
