//! Forward sub-agent and background-process lifecycle events to ACP clients.
//!
//! A session-scoped observer receives domain status snapshots and sends them
//! as VTCode task-lifecycle notifications.

mod messages;
#[cfg(test)]
mod tests;

use self::messages::send_task_lifecycle;
use super::lody::lody_task_id;
use hashbrown::HashSet;
use vtcode_core::subagents::SubagentProgressEvent;
type LifecycleUpdate = SubagentProgressEvent;
#[cfg(test)]
mod projection_tests;
use super::super::types::{SessionHandle, lock_poison_error};
use super::ZedAgent;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tracing::Instrument;
use tracing::{Span, debug, field, info_span, warn};
use vtcode_core::metrics::{TaskLifecycleNotificationQueueDrop, TaskLifecycleNotificationQueueMetricsObserver};
use vtcode_core::subagents::{BackgroundSubprocessEntry, SubagentProgressObserver, SubagentStatusEntry};

const SESSION_CORRELATION_HEX_LENGTH: usize = 12;
const TASK_LIFECYCLE_SEND_FAILURE_CATEGORY: &str = "notification_send_failed";

fn bounded_session_correlation(session_id: &acp::SessionId) -> String {
    vtcode_commons::utils::calculate_sha256(session_id.to_string().as_bytes())
        .chars()
        .take(SESSION_CORRELATION_HEX_LENGTH)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleQueueDropKind {
    Full,
    Closed,
}

impl LifecycleQueueDropKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Closed => "closed",
        }
    }
}

fn lifecycle_queue_drop_kind<T>(error: TrySendError<T>) -> LifecycleQueueDropKind {
    match error {
        TrySendError::Full(_) => LifecycleQueueDropKind::Full,
        TrySendError::Closed(_) => LifecycleQueueDropKind::Closed,
    }
}

#[derive(Default)]
struct LifecycleQueueState {
    depth: usize,
    closed: bool,
}

struct LifecycleQueueTracker {
    state: Mutex<LifecycleQueueState>,
    metrics: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>,
}

impl LifecycleQueueTracker {
    fn new(metrics: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>) -> Self {
        Self {
            state: Mutex::new(LifecycleQueueState::default()),
            metrics,
        }
    }

    fn try_send<T>(&self, sender: &mpsc::Sender<T>, update: T) -> Result<(), LifecycleQueueDropKind> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(LifecycleQueueDropKind::Closed);
        }

        match sender.try_send(update) {
            Ok(()) => {
                state.depth = state.depth.saturating_add(1);
                self.metrics.queue_item_enqueued();
                Ok(())
            }
            Err(error) => Err(lifecycle_queue_drop_kind(error)),
        }
    }

    fn dequeued(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.depth > 0 {
            state.depth -= 1;
            self.metrics.queue_item_dequeued();
        }
    }

    fn record_drop(&self, kind: LifecycleQueueDropKind) -> u64 {
        match kind {
            LifecycleQueueDropKind::Full => self.metrics.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Full),
            LifecycleQueueDropKind::Closed => {
                self.metrics.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Closed)
            }
        }
    }

    fn close_and_drain(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        let remaining = std::mem::take(&mut state.depth);
        if remaining > 0 {
            self.metrics.queue_items_drained(remaining);
        }
    }
}

struct LifecycleQueueForwarderGuard(Arc<LifecycleQueueTracker>);

impl Drop for LifecycleQueueForwarderGuard {
    fn drop(&mut self) {
        self.0.close_and_drain();
    }
}

struct TaskLifecycleForwarderSpan {
    span: Span,
    started_at: Instant,
    notifications_sent: u64,
    send_failures: u64,
    finished: bool,
}

impl TaskLifecycleForwarderSpan {
    fn new(span: Span) -> Self {
        Self {
            span,
            started_at: Instant::now(),
            notifications_sent: 0,
            send_failures: 0,
            finished: false,
        }
    }

    fn record_sent(&mut self) {
        self.notifications_sent = self.notifications_sent.saturating_add(1);
    }

    fn record_failure(&mut self) {
        self.send_failures = self.send_failures.saturating_add(1);
    }

    fn finish(&mut self, outcome: &'static str) {
        if self.finished {
            return;
        }
        let _outcome_recorded = self.span.record("outcome", field::display(outcome));
        let _elapsed_ms_recorded = self
            .span
            .record("elapsed_ms", u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX));
        let _notifications_sent_recorded = self.span.record("notifications_sent", self.notifications_sent);
        let _send_failures_recorded = self.span.record("send_failures", self.send_failures);
        self.finished = true;
    }
}

impl Drop for TaskLifecycleForwarderSpan {
    fn drop(&mut self) {
        self.finish("cancelled");
    }
}

fn task_lifecycle_forwarder_outcome(send_failures: u64) -> &'static str {
    if send_failures == 0 { "completed" } else { "send_failed" }
}

fn warn_task_lifecycle_notification_failure(session_correlation: &str, notification_send_elapsed_ms: u64) {
    warn!(
        session_correlation,
        failure_category = TASK_LIFECYCLE_SEND_FAILURE_CATEGORY,
        notification_send_elapsed_ms,
        "Failed to forward ACP task lifecycle notification"
    );
}

impl ZedAgent {
    pub(super) fn ensure_task_lifecycle_forwarder(&self, session: &SessionHandle) -> anyhow::Result<()> {
        let already_running = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("checking ACP task lifecycle forwarder state", error))?
            .task_lifecycle_forwarder
            .as_ref()
            .is_some_and(|task| !task.is_finished());
        if already_running {
            return Ok(());
        }

        let Some(controller) = self.session_subagent_controller(session)? else {
            return Ok(());
        };
        let Some(client) = self.client() else {
            return Ok(());
        };
        let session_id = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("reading ACP session ID for task lifecycle forwarding", error))?
            .session_id
            .clone();
        let metrics = session
            .workspace_runtime()?
            .map(|runtime| runtime.local_tool_registry.metrics_collector())
            .unwrap_or_else(|| self.local_tool_registry.metrics_collector());
        let (sender, receiver) = mpsc::channel(128);
        let queue = Arc::new(LifecycleQueueTracker::new(metrics));
        let observer = Arc::new(AcpTaskLifecycleObserver {
            sender,
            queue: Arc::clone(&queue),
            session_id: session_id.to_string(),
        });
        let registration = controller.register_progress_observer(session_id.0.to_string(), observer);
        let forwarder = spawn_task_lifecycle_forwarder(receiver, session_id, client, queue);

        let mut data = match session
            .data
            .lock()
            .map_err(|error| lock_poison_error("registering ACP task lifecycle forwarder", error))
        {
            Ok(data) => data,
            Err(error) => {
                forwarder.abort();
                return Err(error);
            }
        };
        if data.task_lifecycle_forwarder.as_ref().is_some_and(|task| !task.is_finished()) {
            forwarder.abort();
        } else {
            data.task_lifecycle_forwarder = Some(forwarder);
            data.task_lifecycle_registration = Some(registration);
        }
        Ok(())
    }
}

struct AcpTaskLifecycleObserver {
    session_id: String,
    sender: mpsc::Sender<LifecycleUpdate>,
    queue: Arc<LifecycleQueueTracker>,
}

impl SubagentProgressObserver for AcpTaskLifecycleObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.try_send(SubagentProgressEvent::Subagent {
            parent_session_id: self.session_id.clone(),
            task: entry,
        });
    }

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.try_send(SubagentProgressEvent::BackgroundProcess {
            parent_session_id: self.session_id.clone(),
            task: entry,
        });
    }
}

impl AcpTaskLifecycleObserver {
    fn try_send(&self, update: LifecycleUpdate) {
        if let Err(kind) = self.queue.try_send(&self.sender, update) {
            let cumulative_count = self.queue.record_drop(kind);
            warn!(
                queue = "acp_task_lifecycle",
                drop_kind = kind.as_str(),
                cumulative_count,
                "ACP task lifecycle update could not be queued"
            );
        }
    }
}

fn spawn_task_lifecycle_forwarder(
    mut receiver: mpsc::Receiver<LifecycleUpdate>,
    session_id: acp::SessionId,
    client: Arc<ConnectionHandle>,
    queue: Arc<LifecycleQueueTracker>,
) -> JoinHandle<()> {
    let queue_guard = LifecycleQueueForwarderGuard(Arc::clone(&queue));
    let session_correlation = bounded_session_correlation(&session_id);
    let span = info_span!(
        "acp_task_lifecycle_forwarder",
        session_correlation = %session_correlation,
        outcome = field::Empty,
        elapsed_ms = field::Empty,
        notifications_sent = 0_u64,
        send_failures = 0_u64
    );
    tokio::spawn(
        async move {
            let _queue_guard = queue_guard;
            let mut forwarder_span = TaskLifecycleForwarderSpan::new(Span::current());
            let mut emitted_task_ids = HashSet::new();
            while let Some(update) = receiver.recv().await {
                queue.dequeued();
                if update.parent_session_id() != session_id.0.as_ref() {
                    continue;
                }
                let task_id = lody_task_id(&update).to_string();
                let previously_emitted = emitted_task_ids.contains(&task_id);
                let send_started_at = Instant::now();
                let send_result = send_task_lifecycle(&client, &session_id, update, previously_emitted);
                let notification_send_elapsed_ms =
                    u64::try_from(send_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                match send_result {
                    Err(_) => {
                        forwarder_span.record_failure();
                        warn_task_lifecycle_notification_failure(&session_correlation, notification_send_elapsed_ms);
                    }
                    Ok(()) => {
                        let _ = emitted_task_ids.insert(task_id);
                        forwarder_span.record_sent();
                        debug!(notification_send_elapsed_ms, "Forwarded ACP task lifecycle notification");
                    }
                }
            }
            let outcome = task_lifecycle_forwarder_outcome(forwarder_span.send_failures);
            forwarder_span.finish(outcome);
        }
        .instrument(span),
    )
}
