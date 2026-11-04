//! ANSI escape sequence constants and utilities
//!
//! Re-exports from diapason-commons for backward compatibility.
//! See `docs/reference/ansi-in-diapason.md` for workspace usage guidance.

pub use diapason_commons::ansi_codes::*;

pub fn notify_attention_with_terminal_method(
    default_enabled: bool,
    message: Option<&str>,
    method: diapason_config::TerminalNotificationMethod,
) {
    let override_mode = match method {
        diapason_config::TerminalNotificationMethod::Auto | diapason_config::TerminalNotificationMethod::Unknown => {
            NotifyMethodOverride::Auto
        }
        diapason_config::TerminalNotificationMethod::Bel => NotifyMethodOverride::Bell,
        diapason_config::TerminalNotificationMethod::Osc9 => NotifyMethodOverride::Osc9,
    };

    notify_attention_with_mode(default_enabled, message, override_mode);
}
