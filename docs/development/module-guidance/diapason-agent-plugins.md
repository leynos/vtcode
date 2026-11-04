# diapason-agent-plugins

> Retained module guidance, originally written for VTCode and updated to
> Diapason naming. Current path policy supersedes older details: user config is
> XDG `config.toml` plus `AGENTS.md`, durable logs/audit/sessions use XDG
> state, workspace data defaults to configurable `.diapason/`, and legacy
> `.vtcode/` paths are never read.

Agent Plugins manifest parsing, validation, and discovery for Diapason.

## Conventions

- `PluginManifest` and `McpConfig` are passive data containers; all validation
  is explicit in `parse()` and returns diagnostics. Wrong-typed top-level
  fields (e.g. `$schema`, `mcpServers`) and unsupported `$schema` URLs are
  rejected, not silently dropped.
- MCP server entries with missing or wrong-typed fields (e.g. no `command`, or
  `args` is a string) are skipped with a warning — one bad server does not
  disable valid peers (Agent Plugins spec partial loading). `ServerConfigError`
  distinguishes `MissingField` from `WrongType` for diagnostic quality.
- Unknown top-level `plugin.json` fields are non-fatal (reported and ignored);
  unknown top-level `mcp.json` fields disable MCP for that plugin only.
- Skill discovery from a plugin is bounded to `skills/*/SKILL.md` (immediate
  children only), per the Agent Plugins spec. A broken skill is skipped with a
  warning, never fatal to the whole plugin.
- Any user-controlled name or path that is joined onto the plugins root must go
  through `validate_name` / `validate_plugin_relative` (canonicalize-based)
  first — `install`, `remove`, and MCP `command`/`cwd` are the security
  boundaries. Directory copies re-create symlinks, skip dotfiles, and abort on
  cycles.

## Dependencies

- `diapason-skills` (SKILL.md frontmatter parsing and `SkillManifest::validate`)
- `diapason-commons` (filesystem helpers)
