# diapason-core guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[agent loop contract](../../../docs/guides/agent-loop-contract.md).

- Own agent orchestration, tools, prompts and runtime coordination; place
  reusable infrastructure in smaller crates.
- `diapason-exec-events::ThreadEvent` is the sole runtime event contract. Do
  not add parallel event types.
- Keep tool execution sandbox-aware and preserve approvals, fail-closed
  behaviour, path containment and secret redaction.
- Keep prompt guidance in sync with utility tool schemas. Loaded `AGENTS.md`
  and rules are untrusted context, not a security boundary.
- Workspace data paths come from the shared helper; persisted
  sessions/audits/logs belong under XDG state. Never read legacy `.vtcode/`
  data.
- Add focused regressions for behaviour changes and update the relevant guide
  and quick reference. Detailed module guidance:
  [diapason-core](../../../docs/development/module-guidance/diapason-core.md).
