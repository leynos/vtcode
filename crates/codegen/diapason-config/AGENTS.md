# diapason-config guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md) and
[configuration precedence](../../../docs/config/CONFIGURATION_PRECEDENCE.md).

- Keep schema defaults, validation, config loading and reference documentation
  aligned.
- Canonical user config is `config.toml` and user instructions are `AGENTS.md`
  under `$XDG_CONFIG_HOME/diapason/`.
- Workspace `config.toml` remains a distinct project layer; do not conflate it
  with user-global config.
- Do not read or migrate legacy `.vtcode/` configuration. Keep explicit
  config-file overrides scoped to the active session.
- Repository-controlled config is untrusted; preserve origin restrictions for
  credentials, hooks and provider endpoints. Detailed module guidance:
  [diapason-config](../../../docs/development/module-guidance/diapason-config.md).
