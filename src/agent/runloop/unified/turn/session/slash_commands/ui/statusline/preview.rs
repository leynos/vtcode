use diapason_core::config::{StatusLineConfig, StatusLineMode};

pub(super) fn build_statusline_preview(
    draft: &StatusLineConfig,
    git: Option<(&str, bool)>,
    thread_context: Option<&str>,
    model: &str,
) -> String {
    match draft.mode {
        StatusLineMode::Hidden => "hidden mode: status line disabled".to_string(),
        StatusLineMode::Unknown => "unknown mode".to_string(),
        StatusLineMode::Command => {
            let command = draft
                .command
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("(unset)");
            format!("command mode (setup does not execute command): {command}")
        }
        StatusLineMode::Auto => {
            let mut left_parts = Vec::new();
            if let Some((branch, dirty)) = git {
                let trimmed = branch.trim();
                if !trimmed.is_empty() {
                    left_parts.push(if dirty {
                        format!("{trimmed}*")
                    } else {
                        trimmed.to_string()
                    });
                }
            }

            let mut right_parts = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for value in [thread_context, Some(model)] {
                let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
                    continue;
                };
                let key = value.to_ascii_lowercase();
                if seen.insert(key) {
                    right_parts.push(value.to_string());
                }
            }
            if draft.show_clock {
                right_parts.push("14:30:05".to_string());
            }

            if left_parts.is_empty() && right_parts.is_empty() {
                return "auto permission review: waiting for runtime context".to_string();
            }
            if left_parts.is_empty() {
                return format!("auto permission review: {}", right_parts.join(" | "));
            }
            if right_parts.is_empty() {
                return format!("auto permission review: {}", left_parts.join(" | "));
            }
            format!("auto permission review: {} | {}", left_parts.join(" | "), right_parts.join(" | "))
        }
    }
}
