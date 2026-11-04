# diapason-skills

> Retained module guidance, originally written for VTCode and updated to
> Diapason naming. Current path policy supersedes older details: user config is
> XDG `config.toml` plus `AGENTS.md`, durable logs/audit/sessions use XDG
> state, workspace data defaults to configurable `.diapason/`, and legacy
> `.vtcode/` paths are never read.

[Root AGENTS.md](../../../AGENTS.md) | Skill types, discovery, loading,
validation. Partial extraction from diapason-core.

## Key Modules

`types.rs` + `manifest.rs` core types | `authoring.rs` | `bundle.rs` |
`templates.rs` | `container.rs` + `container_validation.rs` |
`context_manager.rs` | `validation.rs` + `validation_report.rs` +
`enhanced_validator.rs` | `native_plugin.rs` | `system.rs` |
`document_processor.rs` | `file_references.rs` | `locations.rs` | `model.rs` |
`command_skills.rs` (including `/webmcp`) | `injection.rs` + `instructions.rs` +
`prompt_integration.rs` + `render.rs` | `versioning.rs`

## Architecture Notes

- **Partial extraction** — diapason-core's `skills/mod.rs` re-exports this crate
  plus keeps local sub-modules.
- `templates/` contains template files; `src/skills/assets/samples/` has
  embedded samples.

## Dependencies

`diapason-commons` (fs, SHA256) | `diapason-config` (skills, `PromptFormat`)

## Coding Conventions

`anyhow::Result`, `serde`, `tracing`. Manifest parsing uses `serde-saphyr` for
YAML frontmatter. Template paths relative to `templates/`.

## Gotchas

- `native_plugin.rs` loads process-level code through `dlopen`; callers must
  establish provenance and explicit approval, and must never trust repository
  plugin roots.
