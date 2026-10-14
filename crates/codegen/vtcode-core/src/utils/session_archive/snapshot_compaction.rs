//! Bounded snapshot serialization for durable session history.

use super::{
    MAX_CLIENT_METADATA_BYTES, MAX_CLIENT_METADATA_ENTRIES, MessageContent, SessionArchiveMetadata,
    SessionContinuationRecommendedAction, SessionMessage, SessionProgress, SessionSnapshot,
    history_persistence_enabled, session_history_settings,
};
use anyhow::Result;
use serde::Serialize;

pub(in crate::utils) fn compact_snapshot_to_max_bytes(
    mut snapshot: SessionSnapshot,
    max_bytes: usize,
) -> Result<SessionSnapshot> {
    let client_metadata_before_compaction = prepare_snapshot_client_metadata(&mut snapshot, max_bytes);
    if max_bytes == 0 {
        return Ok(snapshot);
    }

    compact_snapshot_payload_to_fit(&mut snapshot, max_bytes)?;
    report_client_metadata_compaction(
        &snapshot.metadata.client_metadata,
        &client_metadata_before_compaction,
        max_bytes,
    );

    Ok(snapshot)
}

fn prepare_snapshot_client_metadata(
    snapshot: &mut SessionSnapshot,
    max_bytes: usize,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    bound_client_metadata(&mut snapshot.metadata.client_metadata);
    if max_bytes > 0 {
        return snapshot.metadata.client_metadata.clone();
    }

    let discarded_entries = snapshot.metadata.client_metadata.take().map_or(0, |metadata| metadata.len());
    if discarded_entries > 0 {
        tracing::warn!(
            discarded_entries,
            "Dropped client metadata because the session archive snapshot budget is zero"
        );
    }
    minimize_snapshot_payload(snapshot);
    None
}

fn compact_snapshot_payload_to_fit(snapshot: &mut SessionSnapshot, max_bytes: usize) -> Result<()> {
    while snapshot_serialized_size(snapshot)? > max_bytes && reduce_snapshot_payload_once(snapshot) {}

    if snapshot_serialized_size(snapshot)? > max_bytes {
        minimize_snapshot_payload(snapshot);
        shrink_snapshot_strings_until_fit(snapshot, max_bytes)?;
    }

    ensure_snapshot_fits(snapshot, max_bytes)
}

fn reduce_snapshot_payload_once(snapshot: &mut SessionSnapshot) -> bool {
    trim_oldest_snapshot_entries(snapshot) || strip_snapshot_overhead(snapshot) || shrink_snapshot_strings(snapshot)
}

fn shrink_snapshot_strings_until_fit(snapshot: &mut SessionSnapshot, max_bytes: usize) -> Result<()> {
    while snapshot_serialized_size(snapshot)? > max_bytes {
        if !shrink_snapshot_strings(snapshot) {
            break;
        }
    }
    Ok(())
}

fn ensure_snapshot_fits(snapshot: &SessionSnapshot, max_bytes: usize) -> Result<()> {
    let compacted_size = snapshot_serialized_size(snapshot)?;
    if compacted_size > max_bytes {
        anyhow::bail!(
            "session archive snapshot cannot fit the configured size limit after compaction: {compacted_size} > {max_bytes} bytes"
        );
    }
    Ok(())
}

fn snapshot_serialized_size(snapshot: &SessionSnapshot) -> Result<usize> {
    Ok(serde_json::to_vec(snapshot)?.len())
}

fn report_client_metadata_compaction(
    client_metadata: &Option<serde_json::Map<String, serde_json::Value>>,
    original_metadata: &Option<serde_json::Map<String, serde_json::Value>>,
    max_bytes: usize,
) {
    if client_metadata != original_metadata {
        let original_entries = original_metadata.as_ref().map_or(0, serde_json::Map::len);
        let retained_entries = client_metadata.as_ref().map_or(0, serde_json::Map::len);
        let retained_bytes = client_metadata
            .as_ref()
            .and_then(|metadata| serialized_json_size_at_most(metadata, MAX_CLIENT_METADATA_BYTES))
            .unwrap_or(0);
        tracing::warn!(
            original_entries,
            retained_entries,
            discarded_entries = original_entries.saturating_sub(retained_entries),
            serialized_bytes = retained_bytes,
            max_snapshot_bytes = max_bytes,
            "Reduced client metadata to fit the session archive snapshot size limit"
        );
    }
}

pub(in crate::utils) fn prepare_snapshot_for_write(snapshot: SessionSnapshot) -> Result<Option<SessionSnapshot>> {
    if !history_persistence_enabled() {
        return Ok(None);
    }

    let max_bytes = session_history_settings().max_bytes;
    let snapshot = match max_bytes {
        Some(max_bytes) => compact_snapshot_to_max_bytes(snapshot, max_bytes)?,
        None => {
            let mut snapshot = snapshot;
            bound_client_metadata(&mut snapshot.metadata.client_metadata);
            snapshot
        }
    };

    Ok(Some(snapshot))
}

fn minimize_snapshot_payload(snapshot: &mut SessionSnapshot) {
    snapshot.metadata.client_metadata = None;
    snapshot.messages.clear();
    snapshot.transcript.clear();
    snapshot.distinct_tools.clear();
    snapshot.error_logs.clear();
    if let Some(progress) = snapshot.progress.as_mut() {
        progress.recent_messages.clear();
        progress.tool_summaries.clear();
        progress.token_usage = None;
        progress.max_context_tokens = None;
        progress.loaded_skills.clear();
    }
}

fn trim_oldest_snapshot_entries(snapshot: &mut SessionSnapshot) -> bool {
    let mut changed = false;

    if snapshot.messages.len() > 1 {
        snapshot.messages.remove(0);
        changed = true;
    }

    if snapshot.transcript.len() > 1 {
        snapshot.transcript.remove(0);
        changed = true;
    }

    if let Some(progress) = snapshot.progress.as_mut()
        && progress.recent_messages.len() > 1
    {
        progress.recent_messages.remove(0);
        changed = true;
    }

    changed
}

fn strip_snapshot_overhead(snapshot: &mut SessionSnapshot) -> bool {
    let mut changed = false;

    if !snapshot.transcript.is_empty() {
        snapshot.transcript.clear();
        changed = true;
    }
    if !snapshot.distinct_tools.is_empty() {
        snapshot.distinct_tools.clear();
        changed = true;
    }
    if !snapshot.error_logs.is_empty() {
        snapshot.error_logs.clear();
        changed = true;
    }

    if let Some(progress) = snapshot.progress.as_mut() {
        changed |= strip_progress_overhead(progress);
    }

    changed
}

fn strip_progress_overhead(progress: &mut SessionProgress) -> bool {
    let mut changed = false;

    if !progress.tool_summaries.is_empty() {
        progress.tool_summaries.clear();
        changed = true;
    }
    if progress.token_usage.take().is_some() {
        changed = true;
    }
    if progress.max_context_tokens.take().is_some() {
        changed = true;
    }
    if !progress.loaded_skills.is_empty() {
        progress.loaded_skills.clear();
        changed = true;
    }

    changed
}

fn shrink_snapshot_strings(snapshot: &mut SessionSnapshot) -> bool {
    let mut changed = shrink_snapshot_metadata(&mut snapshot.metadata);

    for transcript in &mut snapshot.transcript {
        changed |= shrink_string(transcript);
    }

    for message in &mut snapshot.messages {
        changed |= shrink_session_message(message);
    }

    for error_log in &mut snapshot.error_logs {
        changed |= shrink_string(&mut error_log.message);
    }

    if let Some(progress) = snapshot.progress.as_mut() {
        for message in &mut progress.recent_messages {
            changed |= shrink_session_message(message);
        }
        if let Some(token_usage) = progress.token_usage.as_mut() {
            changed |= shrink_string(token_usage);
        }
    }

    changed
}

fn shrink_session_message(message: &mut SessionMessage) -> bool {
    let mut changed = false;
    changed |= shrink_message_content(&mut message.content);

    changed |= shrink_message_auxiliary_fields(message);
    changed |= shrink_tool_call_fields(message);

    changed
}

fn shrink_message_auxiliary_fields(message: &mut SessionMessage) -> bool {
    let mut changed = false;

    if let Some(reasoning) = message.reasoning.as_mut() {
        changed |= shrink_string(reasoning);
    }
    if message.reasoning_details.take().is_some() {
        changed = true;
    }
    if let Some(tool_call_id) = message.tool_call_id.as_mut() {
        changed |= shrink_string(tool_call_id);
    }
    if let Some(origin_tool) = message.origin_tool.as_mut() {
        changed |= shrink_string(origin_tool);
    }
    changed
}

fn shrink_tool_call_fields(message: &mut SessionMessage) -> bool {
    let mut changed = false;

    if let Some(tool_calls) = message.tool_calls.as_mut() {
        for tool_call in tool_calls.iter_mut() {
            changed |= shrink_string(&mut tool_call.id);
            changed |= shrink_string(&mut tool_call.call_type);
            if let Some(function) = tool_call.function.as_mut() {
                changed |= shrink_string(&mut function.name);
                changed |= shrink_string(&mut function.arguments);
            }
            if let Some(text) = tool_call.text.as_mut() {
                changed |= shrink_string(text);
            }
            if let Some(thought_signature) = tool_call.thought_signature.as_mut() {
                changed |= shrink_string(thought_signature);
            }
        }
    }

    changed
}

fn shrink_snapshot_metadata(metadata: &mut SessionArchiveMetadata) -> bool {
    let mut changed = false;

    changed |= shrink_client_metadata(&mut metadata.client_metadata);
    changed |= shrink_string(&mut metadata.workspace_label);
    changed |= shrink_string(&mut metadata.workspace_path);
    changed |= shrink_string(&mut metadata.model);
    changed |= shrink_string(&mut metadata.provider);
    changed |= shrink_string(&mut metadata.theme);
    changed |= shrink_string(&mut metadata.reasoning_effort);
    changed |= shrink_optional_string(&mut metadata.debug_log_path);
    changed |= shrink_optional_string(&mut metadata.prompt_cache_lineage_id);
    changed |= shrink_optional_string(&mut metadata.external_thread_id);
    changed |= shrink_optional_string(&mut metadata.parent_session_id);
    changed |= shrink_optional_string(&mut metadata.primary_agent);

    for skill in &mut metadata.loaded_skills {
        changed |= shrink_string(skill);
    }

    if let Some(continuation) = metadata.continuation_metadata.as_mut() {
        if continuation.max_budget_usd_micros == Some(0) {
            continuation.max_budget_usd_micros = None;
            changed = true;
        }
        if continuation.actual_cost_usd_micros == Some(0) {
            continuation.actual_cost_usd_micros = None;
            changed = true;
        }
        if !continuation.summary_available
            && continuation.recommended_action == Some(SessionContinuationRecommendedAction::ContinueFromSummary)
        {
            continuation.recommended_action = None;
            changed = true;
        }
    }

    changed
}

pub(in crate::utils) fn bound_client_metadata(
    metadata: &mut Option<serde_json::Map<String, serde_json::Value>>,
) -> bool {
    let Some(original) = metadata.take() else {
        return false;
    };

    let original_entries = original.len();
    let mut bounded = serde_json::Map::new();
    let mut changed = original_entries > MAX_CLIENT_METADATA_ENTRIES;

    for (index, (key, value)) in original.into_iter().enumerate() {
        if index >= MAX_CLIENT_METADATA_ENTRIES {
            changed = true;
            break;
        }

        bounded.insert(key.clone(), value);
        if serialized_json_size_at_most(&bounded, MAX_CLIENT_METADATA_BYTES).is_none() {
            bounded.remove(&key);
            changed = true;
        }
    }

    let retained_entries = bounded.len();
    let serialized_bytes = serialized_json_size_at_most(&bounded, MAX_CLIENT_METADATA_BYTES).unwrap_or(0);
    if bounded.is_empty() {
        *metadata = (original_entries == 0).then_some(bounded);
    } else {
        *metadata = Some(bounded);
    }

    if changed && original_entries > retained_entries {
        tracing::warn!(
            original_entries,
            retained_entries,
            discarded_entries = original_entries.saturating_sub(retained_entries),
            serialized_bytes,
            max_entries = MAX_CLIENT_METADATA_ENTRIES,
            max_bytes = MAX_CLIENT_METADATA_BYTES,
            "Truncated client metadata to session archive limits"
        );
    }

    changed
}

fn shrink_client_metadata(metadata: &mut Option<serde_json::Map<String, serde_json::Value>>) -> bool {
    let Some(entries) = metadata.as_mut() else {
        return false;
    };
    let Some(last_key) = entries.keys().next_back().cloned() else {
        *metadata = None;
        return true;
    };

    entries.remove(&last_key);
    if entries.is_empty() {
        *metadata = None;
    }
    true
}

pub(in crate::utils) fn serialized_json_size_at_most<T: Serialize>(value: &T, max_bytes: usize) -> Option<usize> {
    struct LimitedWriter {
        bytes_written: usize,
        max_bytes: usize,
    }

    impl std::io::Write for LimitedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let Some(new_size) = self.bytes_written.checked_add(bytes.len()) else {
                return Err(std::io::Error::other("serialized JSON size overflowed"));
            };
            if new_size > self.max_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "serialized JSON exceeded its size limit",
                ));
            }
            self.bytes_written = new_size;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LimitedWriter { bytes_written: 0, max_bytes };
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.bytes_written)
}

fn shrink_message_content(content: &mut MessageContent) -> bool {
    match content {
        MessageContent::Text(text) => shrink_string(text),
        MessageContent::Parts(parts) => {
            let mut changed = false;
            for part in parts.iter_mut() {
                changed |= match part {
                    crate::llm::provider::ContentPart::Text { text } => shrink_string(text),
                    crate::llm::provider::ContentPart::Image { data, mime_type, .. } => {
                        shrink_string(data) | shrink_string(mime_type)
                    }
                    crate::llm::provider::ContentPart::File { filename, file_id, file_data, file_url, .. } => {
                        shrink_optional_string(filename)
                            | shrink_optional_string(file_id)
                            | shrink_optional_string(file_data)
                            | shrink_optional_string(file_url)
                    }
                };
            }
            changed
        }
    }
}

fn shrink_optional_string(value: &mut Option<String>) -> bool {
    value.as_mut().is_some_and(shrink_string)
}

fn shrink_string(value: &mut String) -> bool {
    const MIN_RETAINED_CHARS: usize = 8;
    const TRUNCATION_MARKER: &str = "...";

    if value.len() <= MIN_RETAINED_CHARS + TRUNCATION_MARKER.len() {
        return false;
    }

    let keep_len = (value.len() / 2).max(MIN_RETAINED_CHARS);
    let prefix_len = keep_len.saturating_sub(TRUNCATION_MARKER.len());
    value.truncate(prefix_len);
    value.push_str(TRUNCATION_MARKER);
    true
}
