//! Behavioural coverage for bounded, non-executable tool-input previews.
//!
//! The tests drive the preview state machine directly and through the
//! publication boundary. A property test compares retained prefix,
//! truncation flag, coalescing watermark, and terminal updates against a
//! per-call model around the character cap, while unit tests pin the advisory
//! failure policy.

use super::{
    PREVIEW_PUBLISH_STEP_BYTES, PreviewUpdateSink, STREAMED_TOOL_PREVIEW_MAX_CHARS, StreamedToolInputPreviews,
    publish_preview_updates_via,
};
use crate::acp;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Captures rendered event fields so a test can assert exactly what the
/// production preview warning records.
struct CapturedEvents {
    events: Arc<Mutex<Vec<String>>>,
}

impl Subscriber for CapturedEvents {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = String::new();
        event.record(&mut CapturedFields(&mut fields));
        self.events.lock().expect("captured event lock").push(fields);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct CapturedFields<'a>(&'a mut String);

impl Visit for CapturedFields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        writeln!(self.0, "{}={value:?}", field.name()).expect("writing to a String cannot fail");
    }
}

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

/// Return the title carried by a preview update.
fn preview_title(update: &acp::SessionUpdate) -> &str {
    match update {
        acp::SessionUpdate::ToolCall(update) => &update.title,
        acp::SessionUpdate::ToolCallUpdate(update) => update.fields.title.as_deref().unwrap_or_default(),
        other => panic!("unexpected preview update: {other:?}"),
    }
}

/// Return the tool-call status carried by a preview update.
fn preview_status(update: &acp::SessionUpdate) -> acp::ToolCallStatus {
    match update {
        acp::SessionUpdate::ToolCall(update) => update.status,
        acp::SessionUpdate::ToolCallUpdate(update) => update.fields.status.unwrap_or(acp::ToolCallStatus::Pending),
        other => panic!("unexpected preview update: {other:?}"),
    }
}

/// Return the provider call id carried by a preview update.
fn preview_call_id(update: &acp::SessionUpdate) -> &str {
    match update {
        acp::SessionUpdate::ToolCall(update) => update.tool_call_id.0.as_ref(),
        acp::SessionUpdate::ToolCallUpdate(update) => update.tool_call_id.0.as_ref(),
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

/// Prove every publication phase publishes the updates production sends and
/// logs a stable category without streamed input or transport error text.
#[tokio::test]
async fn preview_publish_failures_log_stable_categories_without_tool_input() {
    const TOOL_INPUT_MARKER: &str = "TOOL_INPUT_MARKER_3f8c";
    let events = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(CapturedEvents { events: Arc::clone(&events) });
    let session_id = acp::SessionId::new("preview-trace");
    let mut sink = RecordingSink { attempts: 0, failures_remaining: usize::MAX };

    let mut previews = StreamedToolInputPreviews::default();
    let start = previews.start("call_1".to_string(), Some("read_file".to_string()));
    publish_preview_updates_via(&mut sink, &session_id, "start", start).await;

    let delta = previews.push_delta("call_1".to_string(), TOOL_INPUT_MARKER);
    publish_preview_updates_via(&mut sink, &session_id, "delta", delta).await;

    for (phase, status, note) in [
        (
            "completed",
            acp::ToolCallStatus::Completed,
            "Provider input streaming completed; execution is handled separately.",
        ),
        ("failed", acp::ToolCallStatus::Failed, "The provider stream failed."),
        ("cancelled", acp::ToolCallStatus::Failed, "Input streaming was cancelled; it was not executed."),
    ] {
        let mut previews = StreamedToolInputPreviews::default();
        let started = previews.start("call_1".to_string(), Some("read_file".to_string()));
        assert_eq!(started.len(), 1, "the {phase} phase starts one preview");
        let streamed = previews.push_delta("call_1".to_string(), TOOL_INPUT_MARKER);
        assert_eq!(streamed.len(), 1, "the {phase} phase streams one delta");
        let terminal = previews.finish(status, note);
        assert_eq!(terminal.len(), 1, "the {phase} phase publishes the finish() terminal update");
        assert_eq!(preview_status(&terminal[0]), status, "{phase} terminal status");
        assert!(
            preview_title(&terminal[0]).contains("input preview (not executed)"),
            "{phase} terminal preview must stay non-executable"
        );
        publish_preview_updates_via(&mut sink, &session_id, phase, terminal).await;
    }

    assert_eq!(sink.attempts, 5, "start, delta, completed, failed and cancelled all published");
    let events = events.lock().expect("captured event lock");
    let logs = events.join("\n");
    for phase in ["start", "delta", "completed", "failed", "cancelled"] {
        assert!(logs.contains(&format!("phase=\"{phase}\"")), "missing {phase}: {logs}");
    }
    assert!(logs.contains("acp_preview_publish_failed"), "{logs}");
    assert!(!logs.contains(TOOL_INPUT_MARKER), "tool input leaked into logs: {logs}");
    assert!(!logs.contains("client disconnected"), "transport error leaked into logs: {logs}");
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

/// Prove a saturated preview stops republishing unchanged bounded text.
#[test]
fn saturated_preview_does_not_repeat_unchanged_updates() {
    let mut previews = StreamedToolInputPreviews::default();
    let _started = previews.start("call_1".to_string(), Some("read_file".to_string()));
    let _filled = previews.push_delta("call_1".to_string(), &"a".repeat(STREAMED_TOOL_PREVIEW_MAX_CHARS));

    let first_overflow = previews.push_delta("call_1".to_string(), "b");
    assert_eq!(first_overflow.len(), 1, "the truncation flag change is published once");
    assert!(preview_text(&first_overflow[0]).contains("[preview truncated]"));

    assert!(previews.push_delta("call_1".to_string(), "c").is_empty());
    assert!(previews.push_delta("call_1".to_string(), "").is_empty());
}

/// Build a deterministic Unicode delta of `chars` scalars.
///
/// The alphabet mixes one-, two- and four-byte scalars so the retained cap
/// is exercised in characters, not bytes.
fn unicode_delta(chars: usize) -> String {
    const ALPHABET: [char; 4] = ['a', 'é', '漢', '🦀'];
    (0..chars).map(|index| ALPHABET[index % ALPHABET.len()]).collect()
}

/// Classify a delta as small, at the character cap, or above it, so a
/// generated history covers below/at/above the truncation boundary.
fn delta_size_class(class: u8) -> usize {
    match class {
        0 => 0,
        1 => STREAMED_TOOL_PREVIEW_MAX_CHARS - 1,
        2 => STREAMED_TOOL_PREVIEW_MAX_CHARS,
        _ => STREAMED_TOOL_PREVIEW_MAX_CHARS + 1,
    }
}

proptest::proptest! {
    /// Compare retained prefix, truncation flag, and per-call terminal
    /// updates with a per-call model across interleaved Unicode histories
    /// sized below, at, and above the character cap.
    #[test]
    fn preview_updates_match_a_per_call_model(
        operations in proptest::collection::vec((0_u8..3, 0_u8..4), 1..12),
    ) {
        let mut previews = StreamedToolInputPreviews::default();
        // Retained input, truncation flag, and the published byte watermark.
        let mut model: [Option<(String, bool, usize)>; 3] = [None, None, None];

        for (call_index, size_class) in operations {
            let call_id = format!("call_{call_index}");
            let delta = unicode_delta(delta_size_class(size_class));
            let started_before = model[usize::from(call_index)].is_some();
            let updates = previews.push_delta(call_id.clone(), &delta);

            let (input, truncated, published) =
                model[usize::from(call_index)].get_or_insert_with(|| (String::new(), false, 0));
            let retained_chars = input.chars().count();
            let mut delta_chars = delta.chars();
            let added: String = delta_chars
                .by_ref()
                .take(STREAMED_TOOL_PREVIEW_MAX_CHARS.saturating_sub(retained_chars))
                .collect();
            input.push_str(&added);
            let overflowed = delta_chars.next().is_some();
            let was_truncated = *truncated;
            *truncated |= overflowed;
            // Deltas are coalesced: the first visible input, a truncation-flag
            // change, or a full publishing step emits a cumulative update. A
            // saturated preview publishes nothing, because neither the retained
            // text nor the truncation flag changed.
            let first_visible = *published == 0 && !input.is_empty();
            let unpublished = input.len().saturating_sub(*published);
            let published_now = first_visible || (*truncated && !was_truncated) || unpublished >= PREVIEW_PUBLISH_STEP_BYTES;
            if published_now {
                *published = input.len();
            }

            let expected_updates = usize::from(!started_before) + usize::from(published_now);
            proptest::prop_assert_eq!(updates.len(), expected_updates);
            if published_now {
                let text = preview_text(updates.last().expect("a published preview emits an update"));
                let truncation_note = if *truncated { "\n\n[preview truncated]" } else { "" };
                let expected = if input.is_empty() {
                    format!("Input is streaming; it has not been executed.{truncation_note}")
                } else {
                    format!("Input is streaming; it has not been executed.\n\n{input}{truncation_note}")
                };
                proptest::prop_assert_eq!(text, expected);
            }
        }

        let terminal = previews.finish(acp::ToolCallStatus::Completed, "Prepared; it was not executed.");
        let started_calls = model.iter().filter(|entry| entry.is_some()).count();
        proptest::prop_assert_eq!(terminal.len(), started_calls);
        let mut terminal_calls = std::collections::BTreeSet::new();
        for update in &terminal {
            let call_id = preview_call_id(update);
            let call_index = call_id
                .strip_prefix("provider-input-preview:call_")
                .and_then(|index| index.parse::<usize>().ok())
                .expect("terminal update must name a generated call");
            proptest::prop_assert!(
                terminal_calls.insert(call_index),
                "each call must receive exactly one terminal update"
            );
            let (input, truncated, _published) = model[call_index]
                .as_ref()
                .expect("terminal update must match a started call");
            proptest::prop_assert_eq!(
                preview_status(update),
                acp::ToolCallStatus::Completed,
                "terminal status must match the finish status"
            );
            let truncation_note = if *truncated { "\n\n[preview truncated]" } else { "" };
            let expected = if input.is_empty() {
                format!("Prepared; it was not executed.{truncation_note}")
            } else {
                format!("Prepared; it was not executed.\n\n{input}{truncation_note}")
            };
            proptest::prop_assert_eq!(
                preview_text(update),
                expected,
                "terminal update for {} must carry its retained prefix and truncation flag",
                call_id
            );
        }
    }
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

    // Deltas are coalesced, so the second one has to cross the publishing
    // step for the retained prefix to be observable in an update.
    let padding = "x".repeat(PREVIEW_PUBLISH_STEP_BYTES);
    let updates = previews.push_delta("call_1".to_string(), &format!("\"vtcode\"}}{padding}"));
    assert_eq!(updates.len(), 1);
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
