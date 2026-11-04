use std::collections::VecDeque;

use diapason_ui::tui::app::{InlineHandle, SubmittedInput};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedInput {
    pub(crate) input: SubmittedInput,
    pub(crate) primary_agent: Option<String>,
    /// Only Ctrl+Enter submissions are batchable; plain Enter queued while
    /// busy must dispatch as its own turn.
    pub(crate) batchable: bool,
}

impl QueuedInput {
    pub(crate) fn new(input: SubmittedInput, primary_agent: Option<String>) -> Self {
        Self {
            batchable: input.batchable,
            input,
            primary_agent: primary_agent.filter(|name| !name.trim().is_empty()),
        }
    }

    fn display_label(&self) -> String {
        match self.primary_agent.as_deref() {
            Some(agent) => format!("{agent}: {}", self.input.text),
            None => self.input.text.clone(),
        }
    }
}

pub(crate) struct InlineQueueState<'a> {
    handle: &'a InlineHandle,
    queued_inputs: &'a mut VecDeque<QueuedInput>,
    prefer_latest_once: &'a mut bool,
}

impl<'a> InlineQueueState<'a> {
    pub(crate) fn new(
        handle: &'a InlineHandle,
        queued_inputs: &'a mut VecDeque<QueuedInput>,
        prefer_latest_once: &'a mut bool,
    ) -> Self {
        Self { handle, queued_inputs, prefer_latest_once }
    }

    pub(crate) fn push(&mut self, input: SubmittedInput, primary_agent: Option<String>) {
        self.queued_inputs.push_back(QueuedInput::new(input, primary_agent));
        self.sync_handle_queue();
    }

    pub(crate) fn take_next_submission(&mut self) -> Option<QueuedInput> {
        let result = if *self.prefer_latest_once {
            *self.prefer_latest_once = false;
            self.queued_inputs.pop_back()
        } else {
            self.queued_inputs.pop_front()
        };
        self.sync_handle_queue();
        result
    }

    /// Pop the next submission plus every following batchable text-only
    /// submission for the same agent, joined into ONE combined prompt so
    /// several queued Ctrl+Enter messages reach the model in a single turn
    /// instead of one turn each. Non-batchable items (plain Enter, attachments,
    /// agent changes) stop the batch and dispatch alone on their own turns.
    pub(crate) fn take_batched_submission(&mut self) -> Option<QueuedInput> {
        // A Ctrl+Enter "run the latest now" promotion must stay a single-turn
        // dispatch: merging the newest item with older FIFO items would batch
        // turns the user asked to run individually.
        let promoted_latest = *self.prefer_latest_once;
        let mut batch = self.take_next_submission()?;
        // Only batchable (Ctrl+Enter) submissions coalesce, and never a
        // submission carrying attachments — the association between an image
        // and its message must stay intact. A plain Enter that reached the
        // front must also dispatch alone and never absorb following items.
        if !batch.batchable || promoted_latest || batch.input.has_attachments() {
            tracing::debug!(
                target: "diapason_ui::queue",
                batched_count = 1,
                text_bytes = batch.input.text.len(),
                "queue submission drained (single, non-batchable)"
            );
            self.sync_handle_queue();
            return Some(batch);
        }
        let primary_agent = batch.primary_agent.clone();
        let mut batched_count = 1usize;
        const MAX_BATCH_ITEMS: usize = 32;
        // Cap the combined prompt so a user hammering Ctrl+Enter hundreds of
        // times cannot build one unbounded message.
        const MAX_BATCH_BYTES: usize = 64 * 1024;
        while batched_count < MAX_BATCH_ITEMS {
            let Some(next) = self.queued_inputs.front() else {
                break;
            };
            if !next.batchable || next.input.has_attachments() || next.primary_agent != primary_agent {
                break;
            }
            // Cap the COMBINED prompt: merging `next` must not exceed the
            // budget, otherwise one large queued message could overshoot it.
            let needs_separator = !batch.input.text.trim().is_empty() && !next.input.text.trim().is_empty();
            let merged_len = batch.input.text.len() + usize::from(needs_separator) * 2 + next.input.text.len();
            if merged_len > MAX_BATCH_BYTES {
                break;
            }
            let Some(next) = self.queued_inputs.pop_front() else {
                break;
            };
            batched_count += 1;
            if needs_separator {
                batch.input.text.push_str("\n\n");
            }
            batch.input.text.push_str(&next.input.text);
        }
        tracing::debug!(
            target: "diapason_ui::queue",
            batched_count,
            text_bytes = batch.input.text.len(),
            "queue submission drained"
        );
        self.sync_handle_queue();
        Some(batch)
    }

    pub(crate) fn prefer_latest_next(&mut self) {
        *self.prefer_latest_once = !self.queued_inputs.is_empty();
    }

    pub(crate) fn edit_latest(&mut self) -> Option<String> {
        let result = self.queued_inputs.pop_back().map(|queued| queued.input.text);
        if result.is_some() {
            *self.prefer_latest_once = false;
        }
        self.sync_handle_queue();
        result
    }

    pub(crate) fn clear(&mut self) {
        self.queued_inputs.clear();
        *self.prefer_latest_once = false;
        self.sync_handle_queue();
    }

    fn sync_handle_queue(&self) {
        self.handle
            .set_queued_inputs(self.queued_inputs.iter().map(QueuedInput::display_label).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_drain_in_fifo_order() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push("first".into(), Some("duck".to_string()));
        queue.push("second".into(), Some("build".to_string()));
        queue.push("third".into(), Some("review".to_string()));

        // The queue is strict FIFO: first queued runs first.
        assert_eq!(queue.take_next_submission().map(|queued| queued.input.text).as_deref(), Some("first"));
        assert_eq!(queue.take_next_submission().map(|queued| queued.input.text).as_deref(), Some("second"));
        assert_eq!(queue.take_next_submission().map(|queued| queued.input.text).as_deref(), Some("third"));
    }

    #[test]
    fn prefer_latest_next_promotes_existing_queue_without_reordering_it() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::from([
            QueuedInput::new("first".into(), Some("duck".to_string())),
            QueuedInput::new("second".into(), Some("build".to_string())),
            QueuedInput::new("third".into(), Some("review".to_string())),
        ]);
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.prefer_latest_next();

        assert_eq!(queue.take_next_submission().map(|queued| queued.input.text).as_deref(), Some("third"));
        assert_eq!(queue.take_next_submission().map(|queued| queued.input.text).as_deref(), Some("first"));
        assert_eq!(queue.take_next_submission().map(|queued| queued.input.text).as_deref(), Some("second"));
    }

    #[test]
    fn take_batched_submission_joins_consecutive_batchable_items() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        for text in ["first", "second", "third"] {
            queue.push(SubmittedInput::new(text, Vec::new()).batchable(), None);
        }

        // The queue is strict FIFO: queued order is preserved inside the batch.
        let batched = queue.take_batched_submission().expect("batched submission");
        assert_eq!(batched.input.text, "first\n\nsecond\n\nthird");
        assert!(queue.take_batched_submission().is_none());
    }

    #[test]
    fn take_batched_submission_stops_at_plain_enter_attachments_or_agent_change() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push("text one".into(), Some("planner".to_string()));
        queue.push(
            SubmittedInput::new("with image", vec![diapason_ui::tui::app::ContentPart::image("img", "image/png")]),
            None,
        );
        queue.push(SubmittedInput::new("text two", Vec::new()).batchable(), None);

        // Strict FIFO: the planner-tagged plain-Enter item runs first alone,
        // then the attachment item alone (it stops batching), then the
        // batchable item alone because the queue behind it is already empty.
        let planner_item = queue.take_batched_submission().expect("planner submission");
        assert_eq!(planner_item.input.text, "text one");

        let with_image = queue.take_batched_submission().expect("attachment submission");
        assert_eq!(with_image.input.text, "with image");

        let batched = queue.take_batched_submission().expect("last submission");
        assert_eq!(batched.input.text, "text two");

        assert!(queue.take_batched_submission().is_none());
    }

    #[test]
    fn take_batched_submission_runs_plain_enter_items_as_their_own_turns() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push(SubmittedInput::new("batch me", Vec::new()).batchable(), None);
        queue.push("plain enter one".into(), None);
        queue.push("plain enter two".into(), None);

        // Strict FIFO: the batchable item runs alone because the plain-Enter
        // item behind it stops the batch, then each plain-Enter item runs as
        // its own turn.
        let first = queue.take_batched_submission().expect("first submission");
        assert_eq!(first.input.text, "batch me");

        let second = queue.take_batched_submission().expect("second submission");
        assert_eq!(second.input.text, "plain enter one");

        let third = queue.take_batched_submission().expect("third submission");
        assert_eq!(third.input.text, "plain enter two");

        assert!(queue.take_batched_submission().is_none());
    }

    #[test]
    fn take_batched_submission_keeps_attachment_head_single() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push(
            SubmittedInput::new("see image", vec![diapason_ui::tui::app::ContentPart::image("img", "image/png")]),
            None,
        );
        queue.push(SubmittedInput::new("follow-up", Vec::new()).batchable(), None);

        // The attachment-carrying head must dispatch alone so the image stays
        // associated with its message; the following batchable item may then
        // run alone (nothing else behind it).
        let first = queue.take_batched_submission().expect("attachment head");
        assert_eq!(first.input.text, "see image");
        assert!(first.input.has_attachments());

        let second = queue.take_batched_submission().expect("follow-up");
        assert_eq!(second.input.text, "follow-up");
        assert!(queue.take_batched_submission().is_none());
    }

    #[test]
    fn take_batched_submission_does_not_merge_after_prefer_latest_promotion() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push(SubmittedInput::new("first", Vec::new()).batchable(), None);
        queue.push(SubmittedInput::new("second", Vec::new()).batchable(), None);
        queue.push(SubmittedInput::new("third", Vec::new()).batchable(), None);

        // Ctrl+Enter with empty input while idle promotes the newest item to
        // run alone NOW; it must not absorb the older items into one batch.
        queue.prefer_latest_next();
        let promoted = queue.take_batched_submission().expect("promoted submission");
        assert_eq!(promoted.input.text, "third");

        // Remaining queue drains in strict FIFO inside one batch.
        let batch = queue.take_batched_submission().expect("remaining batch");
        assert_eq!(batch.input.text, "first\n\nsecond");
        assert!(queue.take_batched_submission().is_none());
    }

    #[test]
    fn take_batched_submission_skips_separator_for_whitespace_items() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push(SubmittedInput::new("first", Vec::new()).batchable(), None);
        queue.push(SubmittedInput::new("   ", Vec::new()).batchable(), None);
        queue.push(SubmittedInput::new("third", Vec::new()).batchable(), None);

        let batch = queue.take_batched_submission().expect("batch");
        // The whitespace-only item is still its own message: no separator is
        // injected for it, but it must remain distinct from "third".
        assert_eq!(batch.input.text, "first   \n\nthird");
    }

    #[test]
    fn take_batched_submission_caps_batch_size() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        // 100 batchable items must not become one unbounded prompt.
        for i in 0..100 {
            queue.push(SubmittedInput::new(format!("msg {i}"), Vec::new()).batchable(), None);
        }

        let batch = queue.take_batched_submission().expect("batch");
        let item_count = batch.input.text.split("\n\n").count();
        assert!(item_count <= 32, "batch must be capped, got {item_count} items");
        assert!(queue.take_batched_submission().is_some(), "leftover items must still drain");
    }

    #[test]
    fn queued_input_keeps_primary_agent_captured_at_queue_time() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        queue.push("first".into(), Some("planner".to_string()));
        queue.push("second".into(), Some("builder".to_string()));

        let first = queue.take_next_submission().expect("first queued input");
        assert_eq!(first.input.text, "first");
        assert_eq!(first.primary_agent.as_deref(), Some("planner"));

        let latest = queue.take_next_submission().expect("second queued input");
        assert_eq!(latest.input.text, "second");
        assert_eq!(latest.primary_agent.as_deref(), Some("builder"));
    }

    #[test]
    fn queued_input_preserves_attachments_in_order() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let mut queued_inputs = VecDeque::new();
        let mut prefer_latest_once = false;
        let mut queue = InlineQueueState::new(&handle, &mut queued_inputs, &mut prefer_latest_once);

        let first = diapason_ui::tui::app::ContentPart::image("first", "image/png");
        let second = diapason_ui::tui::app::ContentPart::image("second", "image/jpeg");
        queue.push(SubmittedInput::new("see images", vec![first.clone(), second.clone()]), None);

        let queued = queue.take_next_submission().expect("queued input");
        assert_eq!(queued.input.text, "see images");
        assert_eq!(queued.input.attachments, vec![first, second]);
    }
}
