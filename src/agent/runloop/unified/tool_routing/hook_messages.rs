use anyhow::Result;
use diapason_core::utils::ansi::{AnsiRenderer, MessageStyle};

use diapason_core::hooks::{HookMessage, HookMessageLevel};

pub(super) fn render_hook_messages(renderer: &mut AnsiRenderer, messages: &[HookMessage]) -> Result<()> {
    for message in messages {
        let text = message.text.trim();
        if text.is_empty() {
            continue;
        }

        let style = match message.level {
            HookMessageLevel::Info => MessageStyle::Info,
            HookMessageLevel::Warning => MessageStyle::Info,
            HookMessageLevel::Error => MessageStyle::Error,
        };

        // Rendering is cosmetic; a single failed render must not abort the
        // hook phase (which would discard every hook's messages and block the
        // tool call). Log and continue.
        if let Err(err) = renderer.line(style, text) {
            tracing::warn!(error = %err, "Failed to render lifecycle hook message");
        }
    }

    Ok(())
}
