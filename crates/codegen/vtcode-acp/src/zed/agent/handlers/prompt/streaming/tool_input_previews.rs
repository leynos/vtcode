//! Non-executable previews of provider tool input.
//!
//! [`StreamedToolInputPreviews`] owns bounded preview state and returns ACP
//! updates; it has no transport dependency. [`publish_preview_updates`] is the
//! orchestration-boundary publisher that logs and counts failed publications
//! without exposing streamed tool input.
use super::*;
use hashbrown::HashMap;
use std::collections::{BTreeMap, btree_map::Entry};

const PREVIEW_PUBLISH_FAILURE_TOTAL: &str = "vtcode.acp.tool_input_preview_publish_failure_total";
const STREAMED_TOOL_PREVIEW_MAX_CHARS: usize = 8_192;
/// Unpublished input that forces a cumulative preview publication.
///
/// Each update carries the whole retained preview, so publishing on every
/// provider delta would send quadratic traffic for a call that fills the cap.
const PREVIEW_PUBLISH_STEP_BYTES: usize = 512;

#[derive(Default)]
pub(super) struct StreamedToolInputPreviews {
    calls: BTreeMap<String, StreamedToolInputPreview>,
}

struct StreamedToolInputPreview {
    name: Option<String>,
    input: String,
    truncated: bool,
    /// Retained byte length carried by the last published delta update.
    published_bytes: usize,
}

impl StreamedToolInputPreviews {
    /// Register a streamed call and return its pending preview update.
    ///
    /// A repeated start for the same call is ignored so the preview keeps the
    /// input already accumulated for it.
    pub(super) fn start(&mut self, call_id: String, name: Option<String>) -> Vec<acp::SessionUpdate> {
        let title = streamed_tool_preview_title(name.as_deref(), "Preparing", false);
        let preview = StreamedToolInputPreview {
            name,
            input: String::new(),
            truncated: false,
            published_bytes: 0,
        };
        match self.calls.entry(call_id.clone()) {
            Entry::Vacant(entry) => {
                let _preview = entry.insert(preview);
            }
            Entry::Occupied(_) => return Vec::new(),
        }
        let update = acp::ToolCall::new(streamed_tool_preview_id(&call_id), title)
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .content(streamed_tool_preview_content("", "Input is streaming; it has not been executed.", false));
        vec![acp::SessionUpdate::ToolCall(update)]
    }

    /// Append one streamed delta and return the updates it produces.
    ///
    /// Input beyond [`STREAMED_TOOL_PREVIEW_MAX_CHARS`] is discarded, not
    /// buffered for later display, and records that truncation happened. Each
    /// update carries the whole retained preview, so deltas are coalesced: the
    /// first visible input, a truncation-flag change, and every
    /// [`PREVIEW_PUBLISH_STEP_BYTES`] of unpublished input publish, while a
    /// saturated preview republishes nothing. [`Self::finish`] sends the full
    /// retained text, so coalescing loses no displayed input.
    pub(super) fn push_delta(&mut self, call_id: String, delta: &str) -> Vec<acp::SessionUpdate> {
        let mut updates = Vec::new();
        if !self.calls.contains_key(&call_id) {
            updates.extend(self.start(call_id.clone(), None));
        }
        let Some(preview) = self.calls.get_mut(&call_id) else {
            return updates;
        };
        let was_truncated = preview.truncated;
        let retained_chars = preview.input.chars().count();
        let mut delta_chars = delta.chars();
        preview.input.extend(
            delta_chars
                .by_ref()
                .take(STREAMED_TOOL_PREVIEW_MAX_CHARS.saturating_sub(retained_chars)),
        );
        preview.truncated |= delta_chars.next().is_some();
        let truncation_changed = preview.truncated != was_truncated;
        let unpublished = preview.input.len().saturating_sub(preview.published_bytes);
        let first_visible = preview.published_bytes == 0 && !preview.input.is_empty();
        if !(first_visible || truncation_changed || unpublished >= PREVIEW_PUBLISH_STEP_BYTES) {
            return updates;
        }
        preview.published_bytes = preview.input.len();
        let fields = acp::ToolCallUpdateFields::new()
            .title(streamed_tool_preview_title(preview.name.as_deref(), "Preparing", false))
            .status(acp::ToolCallStatus::Pending)
            .content(streamed_tool_preview_content(
                &preview.input,
                "Input is streaming; it has not been executed.",
                preview.truncated,
            ));
        let update = acp::ToolCallUpdate::new(streamed_tool_preview_id(&call_id), fields);
        updates.push(acp::SessionUpdate::ToolCallUpdate(update));
        updates
    }

    /// Return one terminal update per preview and clear the accumulated state.
    pub(super) fn finish(&mut self, status: acp::ToolCallStatus, terminal_note: &str) -> Vec<acp::SessionUpdate> {
        let updates = self
            .calls
            .iter()
            .map(|(call_id, preview)| {
                let verb = if status == acp::ToolCallStatus::Completed {
                    "Prepared"
                } else {
                    "Abandoned"
                };
                let fields = acp::ToolCallUpdateFields::new()
                    .title(streamed_tool_preview_title(preview.name.as_deref(), verb, true))
                    .status(status)
                    .content(streamed_tool_preview_content(&preview.input, terminal_note, preview.truncated));
                let update = acp::ToolCallUpdate::new(streamed_tool_preview_id(call_id), fields);
                acp::SessionUpdate::ToolCallUpdate(update)
            })
            .collect();
        self.calls.clear();
        updates
    }
}

/// Narrow publication boundary for preview updates.
///
/// Implementors translate one [`acp::SessionUpdate`] into transport work. The
/// publisher owns the advisory-error policy rather than each transport.
trait PreviewUpdateSink {
    /// Transport error type. The publisher drops the value and logs only the
    /// stable phase and category, so it needs no further bound.
    type Error;

    /// Publish one preview update.
    async fn publish(&mut self, update: acp::SessionUpdate) -> Result<(), Self::Error>;
}

/// The production sink: the ACP session notification channel.
struct AgentPreviewSink<'a> {
    agent: &'a ZedAgent,
    session_id: &'a acp::SessionId,
}

impl PreviewUpdateSink for AgentPreviewSink<'_> {
    type Error = acp::Error;

    /// Publish one preview update, reporting any transport failure.
    async fn publish(&mut self, update: acp::SessionUpdate) -> Result<(), Self::Error> {
        self.agent.send_update(self.session_id, update).await
    }
}

/// Publish preview updates at the ACP orchestration boundary.
///
/// A preview is advisory, so a failed publication never fails the turn. The
/// log and metric carry only the phase and error category; streamed tool input
/// stays out of telemetry.
pub(super) async fn publish_preview_updates(
    agent: &ZedAgent,
    session_id: &acp::SessionId,
    phase: &'static str,
    updates: Vec<acp::SessionUpdate>,
) {
    let mut sink = AgentPreviewSink { agent, session_id };
    publish_preview_updates_via(&mut sink, session_id, phase, updates).await;
}

/// Publish each update in order, recording advisory failures without stopping.
async fn publish_preview_updates_via<S: PreviewUpdateSink>(
    sink: &mut S,
    session_id: &acp::SessionId,
    phase: &'static str,
    updates: Vec<acp::SessionUpdate>,
) {
    for update in updates {
        if sink.publish(update).await.is_err() {
            record_preview_publish_failure(session_id, phase);
        }
    }
}

/// Record one preview publication failure as a metric and a stable log record.
///
/// The transport error is deliberately not logged: an `acp::Error` can carry
/// arbitrary message text, and a preview failure must never expose streamed
/// tool input.
fn record_preview_publish_failure(session_id: &acp::SessionId, phase: &'static str) {
    let mut tags = HashMap::new();
    let _previous = tags.insert("phase".to_string(), phase.to_string());
    vtcode_core::telemetry::perf::record_value(PREVIEW_PUBLISH_FAILURE_TOTAL, 1.0, tags);
    tracing::warn!(
        %session_id,
        phase,
        error_category = "acp_preview_publish_failed",
        "Streamed tool input preview was not published"
    );
}

/// Build the non-executable identifier used for a streamed input preview.
fn streamed_tool_preview_id(call_id: &str) -> String {
    format!("provider-input-preview:{call_id}")
}

/// Build the preview title, which always states that input is not executed.
fn streamed_tool_preview_title(name: Option<&str>, verb: &str, terminal: bool) -> String {
    let name = name.unwrap_or("tool");
    let suffix = if terminal {
        "input preview (not executed)"
    } else {
        "input (not executed)"
    };
    format!("{verb} {name} {suffix}")
}

/// Build bounded preview content with an optional truncation marker.
fn streamed_tool_preview_content(input: &str, note: &str, truncated: bool) -> Vec<acp::ToolCallContent> {
    let truncation_note = if truncated { "\n\n[preview truncated]" } else { "" };
    let text = if input.is_empty() {
        format!("{note}{truncation_note}")
    } else {
        format!("{note}\n\n{input}{truncation_note}")
    };
    vec![acp::ContentBlock::Text(acp::TextContent::new(text)).into()]
}

#[cfg(test)]
mod tests;
