# diapason-auth guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep provider OAuth, MCP OAuth and credential storage as distinct flows; all
  OAuth authorization uses PKCE.
- Canonical credentials live under the XDG config `diapason/auth/` directory;
  never discover credentials in legacy `.vtcode/` paths.
- Preserve secure storage read-back, private file permissions and redacted
  error/debug output.
- Storage-mode-aware APIs must retain the caller's selected mode through reads,
  writes and deletes. Detailed module guidance:
  [diapason-auth](../../../docs/development/module-guidance/diapason-auth.md).
