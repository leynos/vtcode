# diapason-ui

Unified UI framework for Diapason: design system, theme registry, and TUI
framework.

## Overview

`diapason-ui` consolidates the design system, theme registry, and terminal UI
framework into a single workspace crate. Its TUI module provides a publicly
reachable UI-facing Rust path, while host-specific integrations remain in
`diapason-core`.

## Architecture

| Area           | Path      | Description                                                      |
| -------------- | --------- | ---------------------------------------------------------------- |
| Design system  | `design/` | Color conversion, style bridging, layout, diff, panel primitives |
| Theme registry | `theme/`  | ThemeStyles, runtime state, syntax theme resolution              |
| TUI framework  | `tui/`    | Session, widgets, runner, markdown rendering, config             |

## Key Components

### Design System

- Color conversion utilities (RGB, HSL, hex)
- Style bridging between terminal and application styles
- Layout primitives for TUI composition
- Diff visualization components

### Theme Registry

- `ThemeStyles` — runtime theme state management
- Syntax highlighting theme resolution
- Theme preview functionality
- Default and custom theme support

### TUI Framework

- `tui/core_tui/` — full terminal session lifecycle
- `tui/ui/` — reusable widgets (markdown, interactive list)
- `tui/config/constants/` — TUI-specific defaults
- Snapshot tests in `tui/core_tui/widgets/snapshots/`

## Usage

The design system and theme modules are re-exported at the crate root for
backward compatibility:

```rust
pub use design::*;
pub use theme::*;
```

### Behaviour configuration API migration

The native Rust type formerly named `BehaviorConfig` is intentionally renamed to
`BehaviourConfig`; no alias for the old name is retained. Import it through
the public TUI module path:

```rust
use diapason_ui::tui::core_tui::session::config::BehaviourConfig;
```

This is a Rust identifier change only. Persisted session configuration keeps
the TOML table key `[behavior]`, so existing files do not need a migration.

## Notes

- Internal workspace crate (`publish = false`); it is not published to crates.io
- Depends on `diapason-commons` for `anstyle_utils` (gated behind `tui` feature)
- `crossterm` dependency enables `event-stream` and `osc52` features

### Syntax-highlighting dependency

Syntax highlighting uses `syntect` with the bundled dumps and the fancy-regex
engine only: `default-features = false` plus `default-syntaxes`,
`default-themes`, and `regex-fancy`. The YAML, plist, and HTML loaders are not
enabled, because nothing in Diapason loads grammars or themes from files at
runtime.

The workspace redirects `syntect` to `patches/syntect/` through the root
`[patch.crates-io]` table. That vendored tree is syntect 5.3.0 with one change:
its dump codec is `bincode-next` in legacy format rather than the unmaintained
`bincode 1.3.3`. The bundled `.packdump` and `.themedump` assets are unchanged
and decode byte-identically. See `patches/syntect/PROVENANCE.txt` and
[the issue #120 ExecPlan](../execplans/issue-120-syntect-advisories.md).

A `[patch.crates-io]` entry applies to this workspace only. If `diapason-ui`
were ever published and consumed elsewhere, that consumer would resolve
upstream syntect again and reintroduce the advisory; the patch would have to
become a released dependency instead.

## See Also

- [Architecture Guide](../ARCHITECTURE.md) — TUI architecture section
- `diapason-core::ui::tui` — host integration surface for the TUI
