//! Forward sub-agent and background-process lifecycle events to ACP clients.
//!
//! A session-scoped observer receives domain status snapshots and sends them
//! as VTCode task-lifecycle notifications.

mod messages;
#[cfg(test)]
mod tests;

pub(super) use self::messages::LifecycleUpdate;
use self::messages::send_task_lifecycle;
use super::lody::{lody_task_id, owned_child_ids};
use hashbrown::HashSet;
#[cfg(test)]
mod forwarder_duplex_tests;
#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod queue_accounting_tests;
use super::super::types::{SessionHandle, lock_poison_error};
use super::ZedAgent;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use chrono::{DateTime, Utc};
use hashbrown::HashMap;
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

    /// Counts one accepted enqueue while the queue state is already locked.
    ///
    /// Every enqueue path must count under the same lock that publishes the
    /// item, because the forwarder decrements the same counter as soon as it
    /// receives. An item that becomes observable before it is counted lets
    /// that dequeue find a zero depth and leave a permanent over-count behind,
    /// reporting a non-empty queue after the channel has emptied.
    fn record_enqueued_locked(&self, state: &mut LifecycleQueueState) {
        state.depth = state.depth.saturating_add(1);
        self.metrics.queue_item_enqueued();
    }

    fn try_send<T>(&self, sender: &mpsc::Sender<T>, update: T) -> Result<(), LifecycleQueueDropKind> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(LifecycleQueueDropKind::Closed);
        }
        match sender.try_send(update) {
            Ok(()) => {
                self.record_enqueued_locked(&mut state);
                Ok(())
            }
            Err(error) => Err(lifecycle_queue_drop_kind(error)),
        }
    }

    /// Enqueues while waiting for capacity instead of dropping.
    ///
    /// The attach-time snapshot publisher uses this path: a snapshot it drops
    /// is a task the client never learns about, so it waits for room instead.
    /// Capacity is reserved before the state lock is taken, so the synchronous
    /// lock is never held across an await. The closure check, the count and
    /// the publication through the permit then share one critical section,
    /// matching [`Self::try_send`] and keeping dequeues balanced whichever
    /// path produced the item.
    async fn send<T>(&self, sender: &mpsc::Sender<T>, update: T) -> Result<(), LifecycleQueueDropKind> {
        let permit = match sender.reserve().await {
            Ok(permit) => permit,
            Err(_) => return Err(LifecycleQueueDropKind::Closed),
        };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(LifecycleQueueDropKind::Closed);
        }
        self.record_enqueued_locked(&mut state);
        permit.send(update);
        Ok(())
    }

    #[cfg(test)]
    fn depth(&self) -> usize {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).depth
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

/// Ordering key for one task snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LifecycleRevision {
    updated_at: DateTime<Utc>,
    terminal: bool,
}

impl LifecycleRevision {
    fn of(update: &LifecycleUpdate) -> Self {
        Self {
            updated_at: update.updated_at(),
            terminal: update.is_terminal(),
        }
    }

    /// Whether this revision may supersede `latest` on the wire.
    ///
    /// The initial-snapshot publisher runs concurrently with live delivery, so
    /// a snapshot captured before a transition can be queued after it. An
    /// update that predates the last one forwarded is stale and must not roll
    /// the client's task back. At an identical timestamp, a terminal state
    /// still wins: a terminal→non-terminal step there is a replay artefact,
    /// not a transition the controller recorded.
    fn supersedes(self, latest: Option<Self>) -> bool {
        latest.is_none_or(|latest| {
            if self.updated_at != latest.updated_at {
                return self.updated_at > latest.updated_at;
            }
            self.terminal || !latest.terminal
        })
    }
}

/// Tracks the newest revision forwarded for each task in one session.
#[derive(Default)]
struct LifecycleOrdering {
    revisions: HashMap<String, LifecycleRevision>,
}

impl LifecycleOrdering {
    /// Records `update` and reports whether it should be forwarded.
    fn admit(&mut self, update: &LifecycleUpdate) -> bool {
        let revision = LifecycleRevision::of(update);
        let latest = self.revisions.get(lody_task_id(update)).copied();
        if !revision.supersedes(latest) {
            return false;
        }
        // The replaced revision is intentionally discarded: the new one is the
        // only revision that may judge later updates.
        let _ = self.revisions.insert(lody_task_id(update).to_owned(), revision);
        true
    }
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
            sender: sender.clone(),
            queue: Arc::clone(&queue),
            session_id: session_id.to_string(),
        });
        let registration = controller.register_progress_observer(session_id.0.to_string(), observer);
        let forwarder = spawn_task_lifecycle_forwarder(receiver, session_id.clone(), client, Arc::clone(&queue));
        // Background restoration runs before this observer is registered, so
        // tasks that were already active at registration have no queued
        // transition. Publish their current snapshot explicitly: otherwise a
        // resumed task that never changes again stays invisible to the client.
        let initial_sender = sender.clone();
        let initial_session_id = session_id.clone();
        let initial_controller = Arc::clone(&controller);
        let initial_queue = queue;
        let _initial_snapshot_task = tokio::spawn(async move {
            publish_initial_task_snapshots(&initial_controller, &initial_session_id, &initial_sender, &initial_queue)
                .await;
        });

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

/// Queues one snapshot for every task that is still active when the observer
/// is registered.
///
/// Restoration and session attach can publish `Starting`/`Running`
/// transitions before the forwarder exists. Replaying the live snapshot
/// closes that gap without replaying terminal history the client never saw,
/// and because the observer is already registered, any later transition still
/// arrives as a regular update.
async fn publish_initial_task_snapshots(
    controller: &Arc<vtcode_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    sender: &mpsc::Sender<LifecycleUpdate>,
    queue: &LifecycleQueueTracker,
) {
    let mut controllers = vec![Arc::clone(controller)];
    controllers.extend(controller.descendant_controllers().await);

    let mut child_entries = Vec::new();
    for controller in &controllers {
        child_entries.extend(controller.status_entries().await);
    }
    let owned_children = owned_child_ids(session_id.0.as_ref(), &child_entries)
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for entry in child_entries {
        if entry.status.is_terminal() || !owned_children.contains(entry.id.as_str()) {
            continue;
        }
        let update = LifecycleUpdate::Subagent {
            parent_session_id: session_id.0.to_string(),
            task: entry,
        };
        if queue.send(sender, update).await.is_err() {
            return;
        }
    }

    for controller in &controllers {
        for entry in controller.background_status_entries().await {
            let owned = entry.owner_session_id.as_deref() == Some(session_id.0.as_ref());
            if !owned || !entry.status.is_active() {
                continue;
            }
            let update = LifecycleUpdate::BackgroundProcess {
                parent_session_id: session_id.0.to_string(),
                task: entry,
            };
            if queue.send(sender, update).await.is_err() {
                return;
            }
        }
    }
}

struct AcpTaskLifecycleObserver {
    session_id: String,
    sender: mpsc::Sender<LifecycleUpdate>,
    queue: Arc<LifecycleQueueTracker>,
}

impl SubagentProgressObserver for AcpTaskLifecycleObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.try_send(LifecycleUpdate::Subagent {
            parent_session_id: self.session_id.clone(),
            task: entry,
        });
    }

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.try_send(LifecycleUpdate::BackgroundProcess {
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
            let mut ordering = LifecycleOrdering::default();
            while let Some(update) = receiver.recv().await {
                queue.dequeued();
                if update.parent_session_id() != session_id.0.as_ref() {
                    continue;
                }
                if !ordering.admit(&update) {
                    debug!(task_id = lody_task_id(&update), "Dropped superseded ACP task lifecycle update");
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
