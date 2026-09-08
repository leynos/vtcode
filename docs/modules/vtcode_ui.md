# vtcode-ui

Unified UI framework for VT Code: design system, theme registry, and TUI framework.

## Overview

`vtcode-ui` consolidates the design system, theme registry, and terminal UI framework into a single workspace crate. Its TUI module provides a publicly reachable UI-facing Rust path, while host-specific integrations remain in `vtcode-core`.

## Architecture

| Area | Path | Description |
|------|------|-------------|
| Design system | `design/` | Color conversion, style bridging, layout, diff, panel primitives |
| Theme registry | `theme/` | ThemeStyles, runtime state, syntax theme resolution |
| TUI framework | `tui/` | Session, widgets, runner, markdown rendering, config |

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

The native Rust type formerly named `BehaviorConfig` is intentionally renamed
to `BehaviourConfig`; no alias for the old name is retained. Import it through
the public TUI module path:

```rust
use vtcode_ui::tui::core_tui::session::config::BehaviourConfig;
```

This is a Rust identifier change only. Persisted session configuration keeps
the TOML table key `[behavior]`, so existing files do not need a migration.

## Notes

- Internal workspace crate (`publish = false`); it is not published to crates.io
- Depends on `vtcode-commons` for `anstyle_utils` (gated behind `tui` feature)
- `crossterm` dependency enables `event-stream` and `osc52` features

## See Also

- [Architecture Guide](../ARCHITECTURE.md) — TUI architecture section
- `vtcode-core::ui::tui` — host integration surface for the TUI
