//! Terminal notification modes, detection, and emission helpers.

use super::{BEL, OSC};
use once_cell::sync::Lazy;
use std::io::{IsTerminal, Write};

/// Notification preference (rich OSC vs bell-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitlNotifyMode {
    /// Notifications disabled.
    Off,
    /// Bell-only notifications.
    Bell,
    /// Rich terminal notifications using OSC 9 or OSC 777.
    Rich,
}

/// Terminal-specific notification capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalNotifyKind {
    /// No supported escape sequence; emit the audible bell only.
    BellOnly,
    /// OSC 9 notification sequence.
    Osc9,
    /// OSC 777 notification sequence.
    Osc777,
}

/// Explicit terminal notification transport override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyMethodOverride {
    /// Detect the transport from the terminal environment.
    Auto,
    /// Force bell-only output.
    Bell,
    /// Force OSC 9 output.
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

/// Parse the `DIAPASON_HITL_BELL` override value.
fn parse_bell_enabled_override(raw: &str) -> bool {
    !matches!(raw.trim().to_ascii_lowercase().as_str(), "false" | "0" | "off")
}

/// Determine whether the bell should play, honouring an env override.
#[inline]
fn is_bell_enabled(default_enabled: bool) -> bool {
    if let Ok(val) = std::env::var("DIAPASON_HITL_BELL") {
        return parse_bell_enabled_override(&val);
    }
    default_enabled
}

#[inline]
fn emit_bell() {
    print!("{BEL}");
    let _flush_result = std::io::stdout().flush();
}

/// Notify the user, selecting the transport automatically.
#[inline]
pub fn notify_attention(default_enabled: bool, message: Option<&str>) {
    notify_attention_with_mode(default_enabled, message, NotifyMethodOverride::Auto);
}

/// Notify the user, using `method` to override transport selection.
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
        send_rich_notification(resolve_notify_kind(method), message);
    }

    emit_bell();
}

/// Resolve the notification transport for the requested override.
fn resolve_notify_kind(method: NotifyMethodOverride) -> TerminalNotifyKind {
    match method {
        NotifyMethodOverride::Auto => *DETECTED_NOTIFY_KIND,
        NotifyMethodOverride::Bell => TerminalNotifyKind::BellOnly,
        NotifyMethodOverride::Osc9 => TerminalNotifyKind::Osc9,
    }
}

/// Emit the rich notification sequence for the resolved transport.
fn send_rich_notification(kind: TerminalNotifyKind, message: Option<&str>) {
    match kind {
        TerminalNotifyKind::Osc9 => send_osc9_notification(message),
        TerminalNotifyKind::Osc777 => send_osc777_notification(message),
        TerminalNotifyKind::BellOnly => {}
    }
}

/// Parse the `DIAPASON_HITL_NOTIFY` preference value.
fn parse_hitl_notify_mode(raw: &str) -> HitlNotifyMode {
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "off" | "0" | "false" => HitlNotifyMode::Off,
        "bell" => HitlNotifyMode::Bell,
        "rich" | "osc" | "notify" => HitlNotifyMode::Rich,
        _ => HitlNotifyMode::Bell,
    }
}

fn hitl_notify_mode(default_enabled: bool) -> HitlNotifyMode {
    if let Ok(raw) = std::env::var("DIAPASON_HITL_NOTIFY") {
        return parse_hitl_notify_mode(&raw);
    }

    if default_enabled {
        HitlNotifyMode::Rich
    } else {
        HitlNotifyMode::Off
    }
}

/// Parse the `DIAPASON_NOTIFY_KIND` transport override value.
fn parse_terminal_notify_kind(raw: &str) -> TerminalNotifyKind {
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "osc9" => TerminalNotifyKind::Osc9,
        "osc777" => TerminalNotifyKind::Osc777,
        _ => TerminalNotifyKind::BellOnly,
    }
}

fn detect_terminal_notify_kind() -> TerminalNotifyKind {
    if let Ok(explicit_kind) = std::env::var("DIAPASON_NOTIFY_KIND") {
        return parse_terminal_notify_kind(&explicit_kind);
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
    let title = sanitize_notification_text("Diapason");
    let payload = build_osc777_payload(&title, &body);
    print!("{payload}{BEL}");
    let _flush_result = std::io::stdout().flush();
}

fn send_osc9_notification(message: Option<&str>) {
    let body = sanitize_notification_text(message.unwrap_or("Human approval required"));
    let payload = build_osc9_payload(&body);
    print!("{payload}{BEL}");
    let _flush_result = std::io::stdout().flush();
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
mod notify_tests;
