# diapason-ui guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep the design system, themes and TUI framework independent of agent runtime
  policy.
- Built-in theme foreground and accent colours must meet WCAG AA 4.5:1 against
  their background; run the theme nextest filter after changes.
- Preserve accessible contrast, keyboard behaviour and terminal capability
  fallbacks. Detailed module guidance:
  [diapason-ui](../../../docs/development/module-guidance/diapason-ui.md).
