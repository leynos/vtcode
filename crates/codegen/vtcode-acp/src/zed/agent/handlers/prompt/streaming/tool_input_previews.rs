//! Non-executable previews of provider tool input.
use super::*;
use std::collections::{BTreeMap, btree_map::Entry};

#[derive(Default)]
pub(super) struct StreamedToolInputPreviews {
    calls: BTreeMap<String, StreamedToolInputPreview>,
}

struct StreamedToolInputPreview {
    name: Option<String>,
    input: String,
    truncated: bool,
}
const STREAMED_TOOL_PREVIEW_MAX_CHARS: usize = 8_192;

impl StreamedToolInputPreviews {
    pub(super) async fn start(
        &mut self,
        agent: &ZedAgent,
        session_id: &acp::SessionId,
        call_id: String,
        name: Option<String>,
    ) {
        let title = streamed_tool_preview_title(name.as_deref(), "Preparing", false);
        let preview = StreamedToolInputPreview { name, input: String::new(), truncated: false };
        match self.calls.entry(call_id.clone()) {
            Entry::Vacant(entry) => {
                let _preview = entry.insert(preview);
            }
            Entry::Occupied(_) => return,
        }
        let update = acp::ToolCall::new(streamed_tool_preview_id(&call_id), title)
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .content(streamed_tool_preview_content("", "Input is streaming; it has not been executed.", false));
        drop(agent.send_update(session_id, acp::SessionUpdate::ToolCall(update)).await);
    }

    pub(super) async fn push_delta(
        &mut self,
        agent: &ZedAgent,
        session_id: &acp::SessionId,
        call_id: String,
        delta: &str,
    ) {
        if !self.calls.contains_key(&call_id) {
            self.start(agent, session_id, call_id.clone(), None).await;
        }
        let Some(preview) = self.calls.get_mut(&call_id) else {
            return;
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
        drop(agent.send_update(session_id, acp::SessionUpdate::ToolCallUpdate(update)).await);
    }

    pub(super) async fn finish(
        &mut self,
        agent: &ZedAgent,
        session_id: &acp::SessionId,
        status: acp::ToolCallStatus,
        terminal_note: &str,
    ) {
        for (call_id, preview) in &self.calls {
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
            drop(agent.send_update(session_id, acp::SessionUpdate::ToolCallUpdate(update)).await);
        }
        self.calls.clear();
    }
}

fn streamed_tool_preview_id(call_id: &str) -> String {
    format!("provider-input-preview:{call_id}")
}

fn streamed_tool_preview_title(name: Option<&str>, verb: &str, terminal: bool) -> String {
    let name = name.unwrap_or("tool");
    let suffix = if terminal {
        "input preview (not executed)"
    } else {
        "input (not executed)"
    };
    format!("{verb} {name} {suffix}")
}

fn streamed_tool_preview_content(input: &str, note: &str, truncated: bool) -> Vec<acp::ToolCallContent> {
    let truncation_note = if truncated { "\n\n[preview truncated]" } else { "" };
    let text = if input.is_empty() {
        format!("{note}{truncation_note}")
    } else {
        format!("{note}\n\n{input}{truncation_note}")
    };
    vec![acp::ContentBlock::Text(acp::TextContent::new(text)).into()]
}
