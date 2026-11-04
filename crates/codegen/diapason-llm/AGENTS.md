# diapason-llm guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[provider guide](../../../docs/development/ADDING_MODELS.md).

- Keep provider clients and streaming abstractions independent of agent
  orchestration.
- Preserve credential redaction, request policy, retry semantics and structured
  error classification.
- Provider addition updates model catalogues, presets, docs and provider tests
  together. Detailed module guidance:
  [diapason-llm](../../../docs/development/module-guidance/diapason-llm.md).
