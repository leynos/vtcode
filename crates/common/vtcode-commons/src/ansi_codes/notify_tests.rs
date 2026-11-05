//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn terminal_mapping_is_deterministic() {
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-kitty", "", false, false, false, false),
        TerminalNotifyKind::Osc777
    );
    // Ghostty doesn't support OSC 9/777, use bell-only to avoid "unknown error"
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-ghostty", "ghostty", false, false, false, false),
        TerminalNotifyKind::BellOnly
    );
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-256color", "wezterm", false, false, false, false),
        TerminalNotifyKind::Osc9
    );
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-256color", "", false, false, false, true),
        TerminalNotifyKind::Osc777
    );
    assert_eq!(
        detect_terminal_notify_kind_from("xterm-256color", "", false, false, false, false),
        TerminalNotifyKind::BellOnly
    );
}

#[test]
fn osc_payload_format_is_stable() {
    assert_eq!(build_osc9_payload("done"), format!("{OSC}9;done"));
    assert_eq!(build_osc777_payload("VT Code", "finished"), format!("{OSC}777;notify;VT Code;finished"));
}

#[test]
fn notification_sanitization_does_not_split_utf8() {
    let raw = format!("{}界", "a".repeat(199));

    assert_eq!(sanitize_notification_text(&raw), "a".repeat(199));
}
