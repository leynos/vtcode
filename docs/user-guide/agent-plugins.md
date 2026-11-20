# Agent Plugins

Agent Plugins are portable packages that bundle Agent Skills and MCP servers
under a root `plugin.json` manifest, following the
[Agent Plugins](https://agent-plugins.org/specification) spec. Install one and
its skills and MCP servers become available to Diapason immediately.

For the full reference — plugin layout, manifest fields, MCP transports, and
security behaviour — see the [Agent Plugins Guide](../guides/agent-plugins.md).

## What a plugin gives you

A plugin directory looks like:

```text
my-plugin/
├── plugin.json              # required: $schema + name
├── skills/
│   └── my-skill/
│       └── SKILL.md         # Agent Skill (name must match its folder)
└── mcp.json                 # optional MCP servers
```

- **Skills** placed under `skills/*/SKILL.md` are discovered from
  `<workspace>/.agents/plugins` and appear alongside your regular skills.
- **MCP servers** declared in `mcp.json` are started at session startup and
  exposed as `<plugin>.<server>` providers.

## Install a plugin

```bash
diapason plugins add https://github.com/example/my-plugin
```

`add` accepts a git URL or a local directory, clones/copies it into
`~/.agents/plugins/<name>`, and validates it. Pass `--name` to override the
directory name:

```bash
diapason plugins add https://github.com/example/my-plugin --name my-tools
```

`add` installs to your **user** plugin root (`~/.agents/plugins`). To keep a
plugin project-local instead, place it (or clone it) under
`<workspace>/.agents/plugins/<name>` yourself.

## Manage plugins

```bash
diapason plugins list                   # installed plugins (skills + MCP counts)
diapason plugins info my-plugin         # details: skills, MCP servers, root
diapason plugins validate ./my-plugin   # check a plugin directory
diapason plugins remove my-plugin       # uninstall from ~/.agents/plugins
```

Inside an interactive session the same management is available through the
`/plugin` slash command:

```text
/plugin                      # open the interactive plugin manager (TUI)
/plugin list                 # list installed plugins
/plugin info <name>          # show plugin details
/plugin add <source> [--name <id>]   # install from a git URL or local directory
/plugin remove <name>        # uninstall a plugin
/plugin validate <path>      # validate a plugin without installing
/plugin refresh              # re-discover plugin MCP providers
/plugin help                 # show command help
```

`/plugin add` behaves exactly like `diapason plugins add` — it installs into
`~/.agents/plugins/<name>` and validates the manifest on the way in.

## Use plugin skills and MCP

Once installed, no further setup is needed:

- **Plugin skills** are loaded into the agent's skill catalogue at session start
  and are available to the agent like any other skill (ask it to use one by
  name).
- **Plugin MCP servers** appear in `/mcp` as `<plugin>.<server>` and connect
  automatically at session start.

Plugin skills and MCP servers are discovered from both
`<workspace>/.agents/plugins` and `~/.agents/plugins`.

## Example: VTCode upstream plugin collection

[`vinhnx/vtcode-plugins`](https://github.com/vinhnx/vtcode-plugins) is a plugin
collection from the VTCode upstream project. It is not a Diapason package, and
its contents and compatibility are maintained by that upstream project.

```bash
# Install the upstream collection and inspect it
diapason plugins add https://github.com/vinhnx/vtcode-plugins.git
diapason plugins list
diapason plugins info vtcode-plugins

# Then in a session, ask the agent to use one of its skills,
# e.g. "Use the rust-skills skill"
```

## Configure plugin behaviour

The `tools.plugins` section of `config.toml` governs the separate **marketplace
tool plugin** runtime (TOML manifests registered via the `manifests` list):

```toml
[tools.plugins]
enabled = true                # toggle the plugin runtime
default_trust = "sandbox"     # sandbox | trusted | untrusted
allow = ["my-plugin"]         # optional allow-list
deny = []                     # optional block-list
auto_reload = true            # hot-reload manifest polling
```

This section does **not** gate Agent Plugins. Agent Plugins
(`~/.agents/plugins` and `<workspace>/.agents/plugins`) are always discovered
and loaded regardless of `tools.plugins` settings.

See the [Configuration Field Reference](../config/CONFIG_FIELD_REFERENCE.md)
for every `tools.plugins.*` option.

## Further reading

- [Agent Plugins Guide](../guides/agent-plugins.md) — reference: layout,
  manifest validation, MCP transports, environment expansion, path containment
- [MCP Integration Guide](../guides/mcp-integration.md) — using MCP servers in
  general
- [Agent Skills Guide](../skills/SKILLS_GUIDE.md) — creating and loading skills
