# diapason-webmcp guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[WebMCP guide](../../../docs/development/webmcp.md).

- Bind to loopback by default; require exact configured origins, expiring
  one-time pairing codes and authenticated sessions.
- Keep workspace adapters bounded and symlink-safe. Browser proposals are not
  writes until runtime policy authorizes them.
- Do not claim a hosted Diapason client exists; document local/self-hosted
  browser use only. Detailed module guidance:
  [diapason-webmcp](../../../docs/development/module-guidance/diapason-webmcp.md).
