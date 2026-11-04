use super::*;
use diapason_core::tools::SpooledOutputReference;

fn structured_tool_preview_from_spool(value: &Value, _workspace_root: &Path) -> Option<String> {
    let reference = SpooledOutputReference::from_value(value)?;
    let obj = value.as_object()?;
    let mut parts = Vec::new();
    if let Some(stderr) = obj
        .get("stderr_preview")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(normalize_whitespace(stderr));
    }

    let is_exec_like = obj.get("exit_code").is_some()
        || obj.get("stderr_preview").is_some()
        || obj.get("result_ref_only").and_then(Value::as_bool) == Some(true)
        || obj.get("spool_ref_only").and_then(Value::as_bool) == Some(true);

    if is_exec_like {
        if let Some(preview) = reference.preview {
            parts.push(format!("Spool excerpt: {}", normalize_spool_preview(preview)));
        } else {
            parts.push(format_spool_reference(&reference, obj));
        }
    } else {
        let mut excerpt_parts = Vec::new();
        if let Some(path) = obj
            .get("source_path")
            .and_then(Value::as_str)
            .or_else(|| obj.get("path").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            excerpt_parts.push(format!("source_path: {path}"));
        }
        if let Some(preview) = reference.preview {
            excerpt_parts.push(format!("Spool excerpt: {}", normalize_spool_preview(preview)));
        } else {
            excerpt_parts.push(format_spool_reference(&reference, obj));
        }
        parts.push(excerpt_parts.join(" | "));
    }
    (!parts.is_empty()).then(|| parts.join(" | "))
}

fn normalize_spool_preview(preview: &str) -> String {
    // A preview may itself have come from an older recovery diagnostic. Keep
    // one stable label at the outer boundary so repeated recovery passes do
    // not amplify "Spool excerpt" markers.
    normalize_whitespace(&preview.replace("Spool excerpt:", "Output excerpt:"))
}

fn format_spool_reference(reference: &SpooledOutputReference<'_>, obj: &serde_json::Map<String, Value>) -> String {
    let bytes = reference
        .original_bytes
        .map_or_else(|| "unknown byte count".to_string(), |bytes| format!("{bytes} bytes"));
    let state = if obj.get("spool_pending").and_then(Value::as_bool) == Some(true) {
        "pending"
    } else if obj.get("spool_complete").and_then(Value::as_bool) == Some(true) {
        "complete"
    } else {
        "state unknown"
    };
    format!("Spool reference: {} ({bytes}; {state})", reference.spool_path)
}

pub(crate) fn build_recovery_context_previews_with_workspace(
    history: &[Message],
    workspace_root: Option<&Path>,
) -> Vec<String> {
    fn truncate_preview(text: &str) -> String {
        if text.chars().count() <= RECOVERY_PREVIEW_MAX_CHARS {
            return text.to_string();
        }
        let end = text
            .char_indices()
            .nth(RECOVERY_PREVIEW_MAX_CHARS)
            .map_or(text.len(), |(i, _)| i);
        let mut t = text[..end].trim_end().to_string();
        t.push_str("...");
        t
    }

    fn preview_line(label: &str, text: &str) -> String {
        format!("{label}: {}", truncate_preview(text))
    }

    fn truncate_to_char_limit(text: &str, limit: usize) -> String {
        if text.chars().count() <= limit {
            return text.to_string();
        }
        if limit <= 3 {
            return text.chars().take(limit).collect();
        }
        let mut truncated = text.chars().take(limit - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }

    fn trimmed_json_str(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
        obj.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_whitespace)
    }

    fn error_preview_text(obj: &serde_json::Map<String, Value>) -> Option<String> {
        match obj.get("error") {
            Some(Value::String(t)) => {
                let t = t.trim();
                (!t.is_empty()).then(|| normalize_whitespace(t))
            }
            Some(Value::Object(e)) => e
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(normalize_whitespace),
            _ => None,
        }
    }

    fn push_unique(parts: &mut Vec<String>, text: Option<String>) {
        if let Some(t) = text
            && !parts.iter().any(|e| e == &t)
        {
            parts.push(t);
        }
    }

    fn compact_json_preview(v: &Value) -> Option<String> {
        let s = serde_json::to_string(v).ok()?;
        let n = normalize_whitespace(&s);
        (!n.is_empty()).then_some(n)
    }

    fn structured_tool_preview(raw_text: &str, workspace_root: Option<&Path>) -> Option<(String, u8)> {
        let value = serde_json::from_str::<Value>(raw_text).ok()?;
        let obj = value.as_object()?;
        let guidance = obj.get("error").and_then(Value::as_object).unwrap_or(obj);
        let mut parts = Vec::new();
        let mut priority = 0u8;
        let matches_array = obj.get("matches").or_else(|| obj.get("results")).and_then(Value::as_array);
        if let Some(matches) = matches_array {
            let path = trimmed_json_str(obj, "path");
            let summary = if matches.is_empty() {
                path.map_or_else(|| "No matches found".to_string(), |p| format!("No matches found in {p}"))
            } else {
                let total = obj
                    .get("total_match_count")
                    .or_else(|| obj.get("matched_count"))
                    .or_else(|| obj.get("count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(matches.len() as u64);
                path.map_or_else(|| format!("Found {total} matches"), |p| format!("Found {total} matches in {p}"))
            };
            push_unique(&mut parts, Some(summary));
            priority = priority.max(20);
        } else if let Some(items) = obj.get("items").and_then(Value::as_array) {
            let total = obj
                .get("total")
                .or_else(|| obj.get("count"))
                .and_then(Value::as_u64)
                .unwrap_or(items.len() as u64);
            push_unique(&mut parts, Some(format!("Listed {total} items")));
            priority = priority.max(10);
        } else if let Some(files) = obj.get("files").and_then(Value::as_array) {
            let total = obj.get("total").and_then(Value::as_u64).unwrap_or(files.len() as u64);
            push_unique(&mut parts, Some(format!("Listed {total} files")));
            priority = priority.max(10);
        }
        push_unique(&mut parts, error_preview_text(obj));
        for key in ["critical_note", "message", "hint"] {
            push_unique(&mut parts, trimmed_json_str(obj, key).or_else(|| trimmed_json_str(guidance, key)));
        }
        if parts
            .iter()
            .any(|p| !(p.starts_with("Listed ") || p.starts_with("Found ") || p.starts_with("No matches found")))
        {
            priority = priority.max(55);
        }
        push_unique(
            &mut parts,
            trimmed_json_str(obj, "next_action")
                .or_else(|| trimmed_json_str(guidance, "next_action"))
                .map(|n| format!("Next action: {n}")),
        );
        if parts.iter().any(|p| p.starts_with("Next action: ")) {
            priority = priority.max(60);
        }
        if let Some(tool) =
            trimmed_json_str(obj, "fallback_tool").or_else(|| trimmed_json_str(guidance, "fallback_tool"))
        {
            let fallback = obj
                .get("fallback_tool_args")
                .or_else(|| guidance.get("fallback_tool_args"))
                .and_then(compact_json_preview)
                .map(|a| format!("Fallback tool: {tool} {a}"))
                .unwrap_or_else(|| format!("Fallback tool: {tool}"));
            push_unique(&mut parts, Some(fallback));
            priority = priority.max(60);
        }
        if let Some(ws) = workspace_root {
            let spool = structured_tool_preview_from_spool(&value, ws);
            if spool.is_some() {
                priority = priority.max(100);
            }
            push_unique(&mut parts, spool);
        }
        if parts.is_empty() {
            for key in ["output", "content", "stdout", "stderr"] {
                push_unique(&mut parts, trimmed_json_str(obj, key));
                if !parts.is_empty() {
                    priority = priority.max(90);
                    break;
                }
            }
        }
        (!parts.is_empty()).then(|| (parts.join(" | "), priority.max(1)))
    }

    let latest_user_request = history.iter().rev().find_map(|m| {
        if m.role != MessageRole::User {
            return None;
        }
        let text = normalize_whitespace(m.content.as_text().trim());
        if text.is_empty() {
            return None;
        }
        Some(preview_line(RECOVERY_PREVIEW_USER_LABEL, &text))
    });

    let mut tool_previews = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (recency_rank, message) in history.iter().rev().enumerate() {
        if message.role != MessageRole::Tool {
            continue;
        }
        let raw_text = message.content.as_text();
        let (text, priority) = structured_tool_preview(raw_text.as_ref(), workspace_root)
            .unwrap_or_else(|| (normalize_whitespace(raw_text.trim()), 50));
        if text.is_empty() || !seen.insert(text.clone()) {
            continue;
        }
        tool_previews.push((text, priority, recency_rank));
    }
    tool_previews.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    tool_previews.truncate(RECOVERY_PREVIEW_MAX_TOOL_OUTPUTS);

    let mut previews = Vec::new();
    if let Some(ur) = latest_user_request {
        previews.push(ur);
    }
    previews.extend(
        tool_previews
            .into_iter()
            .enumerate()
            .map(|(i, (t, _, _))| format!("Tool output {}: {}", i + 1, truncate_preview(&t))),
    );

    if previews.is_empty()
        && let Some(text) = history.iter().rev().find_map(|m| {
            let text = normalize_whitespace(m.content.as_text().trim());
            if text.is_empty() {
                return None;
            }
            let label = match m.role {
                MessageRole::Tool => RECOVERY_PREVIEW_TOOL_LABEL,
                MessageRole::Assistant => RECOVERY_PREVIEW_ASSISTANT_LABEL,
                MessageRole::User => RECOVERY_PREVIEW_USER_LABEL,
                _ => return None,
            };
            Some(preview_line(label, &text))
        })
    {
        previews.push(text);
    }
    let mut bounded_previews = Vec::with_capacity(previews.len());
    let mut total_chars = 0usize;
    for preview in previews {
        let separator_chars = usize::from(!bounded_previews.is_empty());
        let available = RECOVERY_PREVIEW_MAX_TOTAL_CHARS
            .saturating_sub(total_chars)
            .saturating_sub(separator_chars);
        if available == 0 {
            break;
        }
        let preview = truncate_to_char_limit(&preview, available);
        total_chars = total_chars
            .saturating_add(separator_chars)
            .saturating_add(preview.chars().count());
        bounded_previews.push(preview);
    }
    bounded_previews
}
