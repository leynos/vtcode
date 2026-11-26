//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn terminal_mapping_kitty_is_osc777() {
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-kitty", "", false, false, false, false),
        TerminalNotifyKind::Osc777
    );
}

// Ghostty doesn't support OSC 9/777, use bell-only to avoid "unknown error"
#[test]
fn terminal_mapping_ghostty_is_bell_only() {
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-ghostty", "ghostty", false, false, false, false),
        TerminalNotifyKind::BellOnly
    );
}

#[test]
fn terminal_mapping_wezterm_is_osc9() {
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-256color", "wezterm", false, false, false, false),
        TerminalNotifyKind::Osc9
    );
}

#[test]
fn terminal_mapping_vte_is_osc777() {
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-256color", "", false, false, false, true),
        TerminalNotifyKind::Osc777
    );
}

#[test]
fn terminal_mapping_unknown_falls_back_to_bell() {
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-256color", "", false, false, false, false),
        TerminalNotifyKind::BellOnly
    );
}

#[test]
fn osc_payload_format_is_stable() {
    assert_eq!(build_osc9_payload("done"), format!("{OSC}9;done"));
    assert_eq!(build_osc777_payload("Diapason", "finished"), format!("{OSC}777;notify;Diapason;finished"));
}

#[test]
fn notification_sanitization_does_not_split_utf8() {
    let raw = format!("{}界", "a".repeat(199));

    assert_eq!(sanitize_notification_text(&raw), "a".repeat(199));
}

#[test]
fn notification_sanitization_strips_control_characters() {
    assert_eq!(sanitize_notification_text("a\x1b]0;x"), "a]0:x");
}

#[test]
fn notification_sanitization_strips_del_character() {
    assert_eq!(sanitize_notification_text("a\x7fb"), "ab");
}

#[test]
fn notification_sanitization_replaces_semicolon() {
    assert_eq!(sanitize_notification_text("a;b"), "a:b");
}

#[test]
fn notify_method_override_resolves_transport() {
    // `Auto` defers to the cached environment probe, so only the explicit
    // overrides are deterministic here.
    assert_eq!(resolve_notify_kind(NotifyMethodOverride::Bell), TerminalNotifyKind::BellOnly);
    assert_eq!(resolve_notify_kind(NotifyMethodOverride::Osc9), TerminalNotifyKind::Osc9);
}

#[test]
fn hitl_notify_mode_parsing_recognizes_aliases() {
    assert_eq!(parse_hitl_notify_mode("off"), HitlNotifyMode::Off);
    assert_eq!(parse_hitl_notify_mode("0"), HitlNotifyMode::Off);
    assert_eq!(parse_hitl_notify_mode("false"), HitlNotifyMode::Off);
    assert_eq!(parse_hitl_notify_mode(" BELL "), HitlNotifyMode::Bell);
    assert_eq!(parse_hitl_notify_mode("rich"), HitlNotifyMode::Rich);
    assert_eq!(parse_hitl_notify_mode("osc"), HitlNotifyMode::Rich);
    assert_eq!(parse_hitl_notify_mode("notify"), HitlNotifyMode::Rich);
    assert_eq!(parse_hitl_notify_mode("unknown"), HitlNotifyMode::Bell);
}

#[test]
fn bell_override_parsing_disables_only_on_explicit_false() {
    assert!(!parse_bell_enabled_override("false"));
    assert!(!parse_bell_enabled_override("0"));
    assert!(!parse_bell_enabled_override("off"));
    assert!(!parse_bell_enabled_override(" FALSE "));
    assert!(parse_bell_enabled_override("true"));
    assert!(parse_bell_enabled_override("1"));
    assert!(parse_bell_enabled_override(""));
}

#[test]
fn notify_kind_override_parsing_recognizes_transports() {
    assert_eq!(parse_terminal_notify_kind("osc9"), TerminalNotifyKind::Osc9);
    assert_eq!(parse_terminal_notify_kind(" OSC777 "), TerminalNotifyKind::Osc777);
    assert_eq!(parse_terminal_notify_kind("bell"), TerminalNotifyKind::BellOnly);
    assert_eq!(parse_terminal_notify_kind("off"), TerminalNotifyKind::BellOnly);
    assert_eq!(parse_terminal_notify_kind("unknown"), TerminalNotifyKind::BellOnly);
}
