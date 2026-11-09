# diapason-mcp guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[MCP guide](../../../docs/guides/mcp-integration.md).

- Keep MCP client/server protocol behaviour separate from agent orchestration.
- Validate remote endpoints and keep credentials out of logs and errors.
- Preserve cancellation, bounded responses and explicit connection lifecycle
  behaviour. Detailed module guidance:
  [diapason-mcp](../../../docs/development/module-guidance/diapason-mcp.md).
