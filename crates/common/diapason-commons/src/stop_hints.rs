//! Reusable hints advertising how to stop an in-flight turn.

/// Compact stop hint suitable for status lines.
pub const STOP_HINT_COMPACT: &str = "Esc stop • Ctrl+C stop • /stop";
/// Stop hint suitable for inline prompts.
pub const STOP_HINT_INLINE: &str = "Esc, Ctrl+C, or /stop to stop";

/// Append the inline stop hint to `message`.
pub fn with_stop_hint(message: &str) -> String {
    if message.trim().is_empty() {
        STOP_HINT_INLINE.to_string()
    } else {
        format!("{message} ({STOP_HINT_INLINE})")
    }
}
