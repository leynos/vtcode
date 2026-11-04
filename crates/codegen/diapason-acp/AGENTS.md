# diapason-acp guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[ACP integration](../../../docs/acp/ACP_INTEGRATION.md).

- ACP is the public protocol and session naming surface. Keep persisted session
  filenames protocol-neutral or named `acp` as required.
- Zed remains a supported ACP client integration; retain its client-specific
  adapters, IDs and configuration where they identify the actual product.
- Preserve authentication, workspace-root containment, session lifecycle and
  approval boundaries.
- Runtime events use `diapason-exec-events::ThreadEvent`.
Detailed module guidance:
[diapason-acp](../../../docs/development/module-guidance/diapason-acp.md).
