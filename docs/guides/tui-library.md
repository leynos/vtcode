# TUI Library Guide

This guide describes the `diapason-ui` crate and how to use its TUI
functionality from other crates.

## What It Provides

- Stable import surface for Diapason inline terminal UI primitives
- Explicit module split: `diapason_ui::tui::core` for reusable TUI foundation,
  `diapason_ui::tui::app` for Diapason-specific overlays and behaviors
- Standalone session options API (`diapason_ui::tui::app::SessionOptions`,
  `SessionSurface`, `KeyboardProtocolSettings`)
- Session lifecycle APIs (`diapason_ui::tui::app::spawn_session_with_options`,
  `spawn_session_with_host`)
- Typed command/event protocol (`diapason_ui::tui::app::InlineHandle`,
  `InlineCommand`, `InlineEvent`)
- Modal, plan-confirmation, and diff-preview data models

## Current Architecture

`diapason-ui` contains the TUI implementation source in `src/tui/core_tui/`.
For compatibility, `diapason-core::ui::tui` remains the canonical runtime type
surface (compiled through a shim) and re-exports the app-layer API. Standalone
session options still avoid direct `diapason_core::config` imports in
downstream projects.

Implementation source location:

- `crates/codegen/diapason-ui/src/tui/core_tui/` (full TUI modules)
- `crates/codegen/diapason-core/src/ui/tui.rs` (compatibility shim)

## Usage

```rust
use diapason_ui::tui::app::{InlineTheme, SessionOptions, spawn_session_with_options};

# fn run() -> anyhow::Result<()> {
let options = SessionOptions {
    placeholder: Some("Prompt".to_string()),
    ..SessionOptions::default()
};
let _session = spawn_session_with_options(InlineTheme::default(), options)?;
# Ok(()) }
```

## Host Traits

`diapason_ui::tui::host` defines lightweight traits for future host decoupling:

- `WorkspaceInfoProvider`
- `NotificationProvider`
- `ThemeProvider`
- `HostAdapter`

`SessionOptions::from_host` and `spawn_session_with_host` consume these traits
for reusable, host-driven defaults.
