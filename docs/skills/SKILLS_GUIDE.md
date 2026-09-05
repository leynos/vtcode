# Agent Skills Guide

Diapason supports repository, user, admin, and bundled system skills using the
open Agent Skills `SKILL.md` format.

## Discovery

Diapason discovers skills in this order:

1. Nearest ancestor `.agents/skills` from the current working directory up to
   the git repository root
2. `~/.agents/skills`
3. `/etc/codex/skills`
4. Bundled system skills

If multiple skills share the same name, the nearest repository skill wins, then
user, then admin, then system.

Diapason also honours disabled entries from `~/.codex/config.toml`:

```toml
[[skills.config]]
path = "/path/to/skill/SKILL.md"
enabled = false
```

## Skill Structure

Each skill is a directory containing a required `SKILL.md` file.

```text
my-skill/
├── SKILL.md
├── scripts/
├── references/
└── assets/
```

`scripts/`, `references/`, and `assets/` are optional.

## SKILL.md

Diapason accepts the core Agent Skills frontmatter fields plus the client-side
`disable-model-invocation` flag used to hide a skill from the model-facing
startup catalogue while keeping it available for explicit harness activation:

```yaml
---
name: my-skill
description: Explain what this skill does and when to use it.
license: Apache-2.0
compatibility: Requires git and network access
allowed-tools: Read Write Bash
metadata:
  owner: platform-team
---
```

Required fields:

- `name`
- `description`

Optional fields:

- `license`
- `compatibility`
- `metadata`
- `allowed-tools`
- `disable-model-invocation`

Legacy Diapason frontmatter such as `version`, `author`, `when-to-use`,
`when-not-to-use`, `model`, `mode`, `context`, `agent`, `network`,
`permissions`, container flags, and similar extensions is rejected.

Diapason does not support `agents/openai.yaml`. That file is Codex-specific and
ignored by design.

## Prompting Behaviour

- Explicit mention wins: `Use the my-skill skill`
- Implicit matching uses `description`
- Full `SKILL.md` bodies are loaded only when a skill is selected
- Referenced resources are loaded on demand

## Commands

List skills:

```bash
diapason skills list
```

Inspect one skill:

```bash
diapason skills info my-skill
```

Create a new skill scaffold:

```bash
diapason skills create my-skill
```

Validate a skill:

```bash
diapason skills validate ./.agents/skills/my-skill
```

Show configured paths:

```bash
diapason skills config
```

## Slash Command Skills

Diapason also exposes the interactive slash-command surface as skills.

- Canonical skill names use the `cmd-<slash-name>` form, for example
  `cmd-status` or `cmd-review`.
- The `/status` or `/review` slash command remains the primary interactive
  alias.
- Built-in session/UI commands are surfaced as built-in command skills.
- Prompt-oriented slash commands are shipped as bundled system skills in the
  release binary and installed under the system skill cache at runtime.
- Command skills are intentionally excluded from the default prompt-side skill
  index to avoid spending context on slash-command metadata that is already
  exposed elsewhere in the harness.
- Built-in command skills support `info` and `use`, but not `load`.

## Notes

- `diapason skills create` generates a spec-first `SKILL.md` scaffold with
  optional commented guidance for `disable-model-invocation`.
- User-facing skill metadata in Diapason is limited to the strict `SKILL.md`
  fields above.
- Bundled system skills are surfaced as `system` scope.
