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

#[derive(Default)]
pub(super) struct StreamedToolInputPreviews {
    calls: BTreeMap<String, StreamedToolInputPreview>,
}

struct StreamedToolInputPreview {
    name: Option<String>,
    input: String,
    truncated: bool,
}

impl StreamedToolInputPreviews {
    /// Register a streamed call and return its pending preview update.
    ///
    /// A repeated start for the same call is ignored so the preview keeps the
    /// input already accumulated for it.
    pub(super) fn start(&mut self, call_id: String, name: Option<String>) -> Vec<acp::SessionUpdate> {
        let title = streamed_tool_preview_title(name.as_deref(), "Preparing", false);
        let preview = StreamedToolInputPreview { name, input: String::new(), truncated: false };
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
    /// buffered for later display, and records that truncation happened.
    pub(super) fn push_delta(&mut self, call_id: String, delta: &str) -> Vec<acp::SessionUpdate> {
        let mut updates = Vec::new();
        if !self.calls.contains_key(&call_id) {
            updates.extend(self.start(call_id.clone(), None));
        }
        let Some(preview) = self.calls.get_mut(&call_id) else {
            return updates;
        };
        let retained_chars = preview.input.chars().count();
        let mut delta_chars = delta.chars();
        preview.input.extend(
            delta_chars
                .by_ref()
                .take(STREAMED_TOOL_PREVIEW_MAX_CHARS.saturating_sub(retained_chars)),
        );
        preview.truncated |= delta_chars.next().is_some();
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
    /// Transport error type; only its display form reaches the log.
    type Error: Display;

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
        if let Err(error) = sink.publish(update).await {
            record_preview_publish_failure(session_id, phase, error);
        }
    }
}

/// Record one preview publication failure as a metric and a stable log record.
fn record_preview_publish_failure(session_id: &acp::SessionId, phase: &'static str, error: impl Display) {
    let mut tags = HashMap::new();
    let _previous = tags.insert("phase".to_string(), phase.to_string());
    vtcode_core::telemetry::perf::record_value(PREVIEW_PUBLISH_FAILURE_TOTAL, 1.0, tags);
    tracing::warn!(
        %session_id,
        phase,
        error_category = "acp_preview_publish_failed",
        %error,
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
mod tests {
    use super::{
        PreviewUpdateSink, STREAMED_TOOL_PREVIEW_MAX_CHARS, StreamedToolInputPreviews, publish_preview_updates_via,
    };
    use crate::acp;

    /// Records publications and fails a configured number of leading attempts.
    struct RecordingSink {
        attempts: usize,
        failures_remaining: usize,
    }

    impl PreviewUpdateSink for RecordingSink {
        type Error = &'static str;

        /// Forward a preview update to the ACP session notification channel.
        async fn publish(&mut self, _update: acp::SessionUpdate) -> Result<(), Self::Error> {
            self.attempts += 1;
            if self.failures_remaining > 0 {
                self.failures_remaining -= 1;
                Err("client disconnected")
            } else {
                Ok(())
            }
        }
    }

    /// Concatenate the text blocks of a preview update.
    fn preview_text(update: &acp::SessionUpdate) -> String {
        let content = match update {
            acp::SessionUpdate::ToolCall(update) => &update.content,
            acp::SessionUpdate::ToolCallUpdate(update) => update.fields.content.as_deref().unwrap_or_default(),
            other => panic!("unexpected preview update: {other:?}"),
        };
        content
            .iter()
            .filter_map(|content| match content {
                acp::ToolCallContent::Content(content) => match &content.content {
                    acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// Return the tool-call status carried by a preview update.
    fn preview_status(update: &acp::SessionUpdate) -> acp::ToolCallStatus {
        match update {
            acp::SessionUpdate::ToolCall(update) => update.status,
            acp::SessionUpdate::ToolCallUpdate(update) => update.fields.status.unwrap_or(acp::ToolCallStatus::Pending),
            other => panic!("unexpected preview update: {other:?}"),
        }
    }

    /// Prove the publisher sends every update when the sink succeeds.
    #[tokio::test]
    async fn publisher_publishes_every_update_when_the_sink_succeeds() {
        let session_id = acp::SessionId::new("preview-publish-success");
        let mut previews = StreamedToolInputPreviews::default();
        let mut updates = previews.start("call_1".to_string(), Some("read_file".to_string()));
        updates.extend(previews.push_delta("call_1".to_string(), "{\"path\":\"a.txt\"}"));
        let mut sink = RecordingSink { attempts: 0, failures_remaining: 0 };

        publish_preview_updates_via(&mut sink, &session_id, "delta", updates).await;

        assert_eq!(sink.attempts, 2);
    }

    /// Prove the publisher continues after an advisory publication failure.
    #[tokio::test]
    async fn publisher_keeps_publishing_after_an_advisory_failure() {
        let session_id = acp::SessionId::new("preview-publish-failure");
        let mut previews = StreamedToolInputPreviews::default();
        let mut updates = previews.start("call_1".to_string(), Some("read_file".to_string()));
        updates.extend(previews.push_delta("call_1".to_string(), "{\"path\":\"a.txt\"}"));
        let mut sink = RecordingSink { attempts: 0, failures_remaining: 1 };

        publish_preview_updates_via(&mut sink, &session_id, "delta", updates).await;

        assert_eq!(sink.attempts, 2, "a failed preview must not stop later updates");
    }

    /// Prove preview input is bounded and the truncation marker appears once.
    #[test]
    fn preview_input_is_bounded_and_marks_truncation_once() {
        let mut previews = StreamedToolInputPreviews::default();
        let hidden_suffix = "hidden-suffix";
        let long_input = format!("{}{hidden_suffix}", "a".repeat(STREAMED_TOOL_PREVIEW_MAX_CHARS));

        let started = previews.start("call_1".to_string(), Some("search_workspace".to_string()));
        assert_eq!(started.len(), 1);
        let updates = previews.push_delta("call_1".to_string(), &long_input);
        assert_eq!(updates.len(), 1);

        let text = preview_text(&updates[0]);
        assert!(text.contains(&"a".repeat(STREAMED_TOOL_PREVIEW_MAX_CHARS)));
        assert!(text.contains("[preview truncated]"));
        assert!(!text.contains(hidden_suffix));

        let terminal = previews.finish(acp::ToolCallStatus::Completed, "Prepared; it was not executed.");
        assert_eq!(terminal.len(), 1);
        assert_eq!(preview_status(&terminal[0]), acp::ToolCallStatus::Completed);
        assert!(preview_text(&terminal[0]).contains("[preview truncated]"));
    }

    /// Prove a repeated start keeps the accumulated input.
    #[test]
    fn repeated_start_keeps_accumulated_input() {
        let mut previews = StreamedToolInputPreviews::default();
        let first = previews.start("call_1".to_string(), Some("search_workspace".to_string()));
        assert_eq!(first.len(), 1);
        let _deltas = previews.push_delta("call_1".to_string(), "{\"query\":");

        let repeated = previews.start("call_1".to_string(), Some("other_tool".to_string()));
        assert!(repeated.is_empty());

        let updates = previews.push_delta("call_1".to_string(), "\"vtcode\"}");
        let text = preview_text(&updates[0]);
        assert!(text.contains("{\"query\":\"vtcode\"}"));
    }

    /// Prove finish emits one terminal update per preview and clears state.
    #[test]
    fn finish_emits_one_terminal_update_per_preview_and_clears_state() {
        let mut previews = StreamedToolInputPreviews::default();
        let _first = previews.start("call_1".to_string(), Some("read_file".to_string()));
        let _second = previews.start("call_2".to_string(), None);

        let terminal = previews.finish(acp::ToolCallStatus::Failed, "Cancelled before execution.");
        assert_eq!(terminal.len(), 2);
        for update in &terminal {
            assert_eq!(preview_status(update), acp::ToolCallStatus::Failed);
            assert!(preview_text(update).contains("Cancelled before execution."));
        }
        assert!(previews.finish(acp::ToolCallStatus::Failed, "Nothing streamed.").is_empty());
    }
}
