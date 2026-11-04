use diapason_core::core::agent::events::{EventSink, event_sink};
use diapason_core::core::agent::runtime::StreamingLifecycleBridge as CoreStreamingLifecycleBridge;
use diapason_core::exec::events::ThreadEvent;

use crate::agent::runloop::unified::inline_events::harness::HarnessEventEmitter;
use crate::agent::runloop::unified::ui_interaction::StreamProgressEvent;

pub(super) struct HarnessStreamingBridge {
    inner: CoreStreamingLifecycleBridge,
    assistant_output_observed: bool,
}

impl HarnessStreamingBridge {
    pub(super) fn new(emitter: Option<&HarnessEventEmitter>, turn_id: &str, step: usize, attempt: usize) -> Self {
        let event_sink = emitter.cloned().map(harness_event_sink);
        Self {
            inner: CoreStreamingLifecycleBridge::new(event_sink, turn_id, step, attempt),
            assistant_output_observed: false,
        }
    }

    pub(super) fn on_progress(&mut self, event: StreamProgressEvent) {
        if matches!(&event, StreamProgressEvent::OutputDelta(delta) if !delta.trim().is_empty()) {
            self.assistant_output_observed = true;
        }
        self.inner.on_progress(event);
    }

    pub(super) fn abort(&mut self) {
        self.inner.abort();
    }

    pub(super) fn complete_open_items(&mut self) {
        self.inner.complete_open_items();
    }

    pub(super) fn take_streamed_tool_call_items(&mut self) -> hashbrown::HashMap<String, String> {
        self.inner.take_streamed_tool_call_items()
    }

    pub(super) fn assistant_output_observed(&self) -> bool {
        self.assistant_output_observed
    }
}

fn harness_event_sink(emitter: HarnessEventEmitter) -> EventSink {
    event_sink(move |event: &ThreadEvent| {
        let _ = emitter.emit(event.clone());
    })
}
