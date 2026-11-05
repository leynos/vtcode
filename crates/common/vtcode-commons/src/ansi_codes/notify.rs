#![expect(
    clippy::let_underscore_must_use,
    reason = "ANSI output cleanup intentionally ignores the best-effort flush result."
)]

//! Terminal notification modes, detection, and emission helpers.

use super::{BEL, OSC};
use once_cell::sync::Lazy;
use std::io::{IsTerminal, Write};

/// Notification preference (rich OSC vs bell-only)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitlNotifyMode {
    Off,
    Bell,
    Rich,
}

/// Terminal-specific notification capabilities
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalNotifyKind {
    BellOnly,
    Osc9,
    Osc777,
}

/// Explicit terminal notification transport override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyMethodOverride {
    Auto,
    Bell,
    Osc9,
}

static DETECTED_NOTIFY_KIND: Lazy<TerminalNotifyKind> = Lazy::new(detect_terminal_notify_kind);

/// Play the terminal bell when enabled.
#[inline]
pub fn play_bell(enabled: bool) {
    if !is_bell_enabled(enabled) {
        return;
    }
    emit_bell();
}

/// Determine whether the bell should play, honouring an env override.
#[inline]
fn is_bell_enabled(default_enabled: bool) -> bool {
    if let Ok(val) = std::env::var("VTCODE_HITL_BELL") {
        return !matches!(val.trim().to_ascii_lowercase().as_str(), "false" | "0" | "off");
    }
    default_enabled
}

#[inline]
fn emit_bell() {
    print!("{BEL}");
    let _ = std::io::stdout().flush();
}

#[inline]
pub fn notify_attention(default_enabled: bool, message: Option<&str>) {
    notify_attention_with_mode(default_enabled, message, NotifyMethodOverride::Auto);
}

#[inline]
pub fn notify_attention_with_mode(default_enabled: bool, message: Option<&str>, method: NotifyMethodOverride) {
    if !is_bell_enabled(default_enabled) {
        return;
    }

    if !std::io::stdout().is_terminal() {
        return;
    }

    let mode = hitl_notify_mode(default_enabled);
    if matches!(mode, HitlNotifyMode::Off) {
        return;
    }

    if matches!(mode, HitlNotifyMode::Rich) {
        let notify_kind = match method {
            NotifyMethodOverride::Auto => *DETECTED_NOTIFY_KIND,
            NotifyMethodOverride::Bell => TerminalNotifyKind::BellOnly,
            NotifyMethodOverride::Osc9 => TerminalNotifyKind::Osc9,
        };
        match notify_kind {
            TerminalNotifyKind::Osc9 => send_osc9_notification(message),
            TerminalNotifyKind::Osc777 => send_osc777_notification(message),
            TerminalNotifyKind::BellOnly => {} // No-op
        }
    }

    emit_bell();
}

fn hitl_notify_mode(default_enabled: bool) -> HitlNotifyMode {
    if let Ok(raw) = std::env::var("VTCODE_HITL_NOTIFY") {
        let v = raw.trim().to_ascii_lowercase();
        return match v.as_str() {
            "off" | "0" | "false" => HitlNotifyMode::Off,
            "bell" => HitlNotifyMode::Bell,
            "rich" | "osc" | "notify" => HitlNotifyMode::Rich,
            _ => HitlNotifyMode::Bell,
        };
    }

    if default_enabled {
        HitlNotifyMode::Rich
    } else {
        HitlNotifyMode::Off
    }
}

fn detect_terminal_notify_kind() -> TerminalNotifyKind {
    if let Ok(explicit_kind) = std::env::var("VTCODE_NOTIFY_KIND") {
        let explicit = explicit_kind.trim().to_ascii_lowercase();
        return match explicit.as_str() {
            "osc9" => TerminalNotifyKind::Osc9,
            "osc777" => TerminalNotifyKind::Osc777,
            "bell" | "off" => TerminalNotifyKind::BellOnly,
            _ => TerminalNotifyKind::BellOnly,
        };
    }

    let term = std::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    let has_kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    let has_iterm = std::env::var("ITERM_SESSION_ID").is_ok();
    let has_wezterm = std::env::var("WEZTERM_PANE").is_ok();
    let has_vte = std::env::var("VTE_VERSION").is_ok();

    detect_terminal_notify_kind_from(&term, &term_program, has_kitty, has_iterm, has_wezterm, has_vte)
}

fn send_osc777_notification(message: Option<&str>) {
    let body = sanitize_notification_text(message.unwrap_or("Human approval required"));
    let title = sanitize_notification_text("VT Code");
    let payload = build_osc777_payload(&title, &body);
    print!("{payload}{BEL}");
    let _ = std::io::stdout().flush();
}

fn send_osc9_notification(message: Option<&str>) {
    let body = sanitize_notification_text(message.unwrap_or("Human approval required"));
    let payload = build_osc9_payload(&body);
    print!("{payload}{BEL}");
    let _ = std::io::stdout().flush();
}

fn sanitize_notification_text(raw: &str) -> String {
    const MAX_LEN: usize = 200;
    let cleaned = raw.chars().filter(|c| *c >= ' ' && *c != '\u{007f}').collect::<String>();
    let cleaned = crate::formatting::truncate_byte_budget(&cleaned, MAX_LEN, "");
    cleaned.replace(';', ":")
}

fn detect_terminal_notify_kind_from(
    term: &str,
    term_program: &str,
    has_kitty: bool,
    has_iterm: bool,
    has_wezterm: bool,
    has_vte: bool,
) -> TerminalNotifyKind {
    if term.contains("kitty") || has_kitty {
        return TerminalNotifyKind::Osc777;
    }

    // Ghostty doesn't officially support OSC 9 or OSC 777 notifications
    // Use bell-only to avoid "unknown error" messages
    if term_program.contains("ghostty") {
        return TerminalNotifyKind::BellOnly;
    }

    if term_program.contains("iterm")
        || term_program.contains("wezterm")
        || term_program.contains("warp")
        || term_program.contains("apple_terminal")
        || has_iterm
        || has_wezterm
    {
        return TerminalNotifyKind::Osc9;
    }

    if has_vte {
        return TerminalNotifyKind::Osc777;
    }

    TerminalNotifyKind::BellOnly
}

fn build_osc777_payload(title: &str, body: &str) -> String {
    format!("{OSC}777;notify;{title};{body}")
}

fn build_osc9_payload(body: &str) -> String {
    format!("{OSC}9;{body}")
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod redraw_tests;
