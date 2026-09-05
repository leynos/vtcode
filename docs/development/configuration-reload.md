# Configuration reset and live reload

This guide describes the implementation contract for configuration changes
made while VT Code is running.

## Shared reset service

`vtcode_core::config::ConfigService::reset` is the single reset path for the
CLI and settings palette. It resolves the selected layer through the current
`ConfigDefaultsProvider`, validates an optional explicit path against known
layers, rejects symlinks and non-regular files, writes an empty TOML document
atomically, invalidates the workspace cache, and reloads the effective layer
stack. It never touches credential storage.

The CLI maps `vtcode config reset` to the workspace layer, `--global` to the
canonical user layer, and `--project` to the current project profile. An
explicit `--config path` selects the workspace-layer file for that invocation.
Reset remains command-owned during startup so a malformed configuration can be
repaired without provider authentication or model initialization.

## Runtime reload contract

`SimpleConfigWatcher` polls every configuration path that can affect a
workspace, including the explicit session file, system/user paths, project
profiles, workspace fallback/root files, and the theme file. It retains
nonexistent paths so creation and deletion are observable. Reload errors are
recorded separately from the last valid snapshot and must be surfaced as a
warning without replacing that snapshot.

The unified interaction loop applies valid snapshots through the workspace
reload helper. Runtime identity fields selected by CLI/session state remain
stable; safe UI, policy, timeout, MCP, and custom-provider changes are applied
to the active runtime. The open settings palette reloads its draft through the
same layer stack and restores its remembered section/selection.

The settings palette persists a mutation as a field-level update to the
selected file instead of serializing the merged effective snapshot. This
preserves lower-precedence layers: for example, a user custom provider is not
written into a workspace `vtcode.toml` when a workspace-only setting changes.
Provider definitions and endpoint/credential overrides are written to the
canonical user layer unless the session selected an explicit config file.

Normal startup and live reload also use a repair-capable loader for legacy
repository files. If an otherwise parseable workspace or project file still
contains protected provider definitions or endpoint/credential overrides from
an older flattened write, only those fields are removed atomically and the
configuration is loaded again. Malformed or symlinked files remain errors, and
explicitly selected config files are never repaired.

Planning-mode policy changes are repository writes too: entering or leaving
Planning mode must persist only the tool-policy fields in the workspace layer.
The shared repository-safe writer removes trusted custom providers and provider
endpoint/credential overrides, including stale copies left by older releases.

## Verification

Use nextest for focused checks:

```bash
cargo nextest run --locked -p vtcode-config
cargo nextest run --locked -p vtcode-core -E 'test(/reset|settings_reload|selection_memory/)'
RUSTFLAGS='-D warnings' cargo check --locked --tests -p vtcode-config -p vtcode-core -p vtcode
```

Reset and reload tests must cover layer preservation, cache invalidation,
explicit paths, symlink/non-regular-file rejection, malformed edits,
creation/deletion, debounce behaviour, and selection restoration.
