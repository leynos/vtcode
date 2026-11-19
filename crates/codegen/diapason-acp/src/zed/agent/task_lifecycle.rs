//! Forward sub-agent and background-process lifecycle events to ACP clients.
//!
//! A session-scoped observer receives domain status snapshots and forwards each
//! as a standard ACP `session/update` tool call or tool-call update carrying
//! the Lody task snapshot in `_meta.lody.task`; the `_lody/subagents/list`,
//! `_lody/subagents/cancel` and `_lody/subagents/output` management
//! requests are handled by the sibling `lody_management` module.

mod messages;
mod queue_accounting;
mod snapshot_publication;
#[cfg(test)]
mod tests;

pub(super) use self::messages::LifecycleUpdate;
use self::messages::send_task_lifecycle;
use self::queue_accounting::{LifecycleQueueForwarderGuard, LifecycleQueueTracker};
use self::snapshot_publication::publish_initial_task_snapshots;
use super::lody::lody_task_id;
use hashbrown::HashSet;
#[cfg(test)]
mod forwarder_duplex_tests;
#[cfg(test)]
mod ordering_tests;
#[cfg(test)]
mod projection_metadata_tests;
#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod queue_accounting_tests;
use super::super::types::{SessionHandle, lock_poison_error};
use super::ZedAgent;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use diapason_core::metrics::TaskLifecycleNotificationQueueMetricsObserver;
use diapason_core::subagents::{BackgroundSubprocessEntry, SubagentProgressObserver, SubagentStatusEntry};
use hashbrown::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;
use tracing::{Span, debug, field, info_span, warn};

const SESSION_CORRELATION_HEX_LENGTH: usize = 12;
const TASK_LIFECYCLE_SEND_FAILURE_CATEGORY: &str = "notification_send_failed";

pub(super) fn bounded_session_correlation(session_id: &acp::SessionId) -> String {
    diapason_commons::utils::calculate_sha256(session_id.to_string().as_bytes())
        .chars()
        .take(SESSION_CORRELATION_HEX_LENGTH)
        .collect()
}

/// One queued lifecycle item, tagged with its position in the session's
/// transition order.
///
/// The initial-snapshot publisher runs concurrently with live observer
/// delivery, and the controller stamps both from `Utc::now()`, so a snapshot
/// captured before a transition can share that transition's `updated_at` and
/// still be queued after it. Timestamps are therefore not an ordering key. The
/// adapter assigns a monotonic revision when the item enters the queue: the
/// snapshot pass reserves one base revision before the observer is registered,
/// and every live transition takes a strictly higher revision. The forwarder
/// orders by that revision and keeps `updated_at` only for the wire contract.
struct QueuedLifecycleUpdate {
    update: LifecycleUpdate,
    revision: u64,
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
///
/// The adapter owns this counter, not the controller: the controller's
/// `updated_at` is a wall-clock timestamp for the wire contract and two
/// transitions recorded in the same tick share it, so it cannot order them.
/// A revision taken as the item is queued is monotonic across the snapshot
/// pass and the live observer alike.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LifecycleRevision(u64);

impl LifecycleRevision {
    fn of(queued: &QueuedLifecycleUpdate) -> Self {
        Self(queued.revision)
    }

    /// Whether this revision may supersede `latest` on the wire.
    ///
    /// Revisions are assigned strictly increasing per session, so the newer
    /// revision always wins. The initial snapshot shares one base revision
    /// across every task it publishes, which is safe because revisions are
    /// compared per task id and the pass emits at most one snapshot per task.
    fn supersedes(self, latest: Option<Self>) -> bool {
        latest.is_none_or(|latest| self.0 > latest.0)
    }
}

/// Tracks the newest revision forwarded for each task in one session.
#[derive(Default)]
struct LifecycleOrdering {
    revisions: HashMap<String, LifecycleRevision>,
}

impl LifecycleOrdering {
    /// Records `queued` and reports whether it should be forwarded.
    fn admit(&mut self, queued: &QueuedLifecycleUpdate) -> bool {
        let revision = LifecycleRevision::of(queued);
        let task_id = lody_task_id(&queued.update);
        let latest = self.revisions.get(task_id).copied();
        if !revision.supersedes(latest) {
            return false;
        }
        // The replaced revision is intentionally discarded: the new one is the
        // only revision that may judge later updates.
        let _ = self.revisions.insert(task_id.to_owned(), revision);
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
        // The tracker holds the observer as a trait object so tests can
        // substitute a counting double, so the collector Arc is coerced here.
        let queue_metrics = Arc::clone(&metrics) as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
        let queue = Arc::new(LifecycleQueueTracker::new(queue_metrics));
        let next_revision = Arc::new(AtomicU64::new(0));
        // Reserve the snapshot pass's revision before the observer exists, so
        // every live transition registered from here takes a strictly newer
        // revision than the snapshots. A snapshot captured from the same state
        // a transition reports must not overwrite the transition.
        let snapshot_revision = next_revision.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let observer = Arc::new(AcpTaskLifecycleObserver {
            sender: sender.clone(),
            queue: Arc::clone(&queue),
            session_id: session_id.to_string(),
            next_revision: Arc::clone(&next_revision),
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
        let initial_metrics = Arc::clone(&metrics);
        // Keep the publisher's handle: it writes into the forwarder's queue, so
        // aborting only the forwarder would leave it enqueueing into a queue
        // nothing drains. It is stopped with the forwarder in every path.
        let initial_snapshot_task = tokio::spawn(async move {
            publish_initial_task_snapshots(
                &initial_controller,
                &initial_session_id,
                &initial_sender,
                &initial_queue,
                &initial_metrics,
                snapshot_revision,
            )
            .await;
        });

        let mut data = match session
            .data
            .lock()
            .map_err(|error| lock_poison_error("registering ACP task lifecycle forwarder", error))
        {
            Ok(data) => data,
            Err(error) => {
                initial_snapshot_task.abort();
                forwarder.abort();
                return Err(error);
            }
        };
        if data.task_lifecycle_forwarder.as_ref().is_some_and(|task| !task.is_finished()) {
            initial_snapshot_task.abort();
            forwarder.abort();
        } else {
            if let Some(previous_publisher) = data.task_lifecycle_snapshot_publisher.replace(initial_snapshot_task) {
                previous_publisher.abort();
            }
            data.task_lifecycle_forwarder = Some(forwarder);
            data.task_lifecycle_registration = Some(registration);
        }
        Ok(())
    }
}

struct AcpTaskLifecycleObserver {
    session_id: String,
    sender: mpsc::Sender<QueuedLifecycleUpdate>,
    queue: Arc<LifecycleQueueTracker>,
    /// Shared with the initial-snapshot pass: every live transition takes a
    /// revision newer than the pass's reserved base.
    next_revision: Arc<AtomicU64>,
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
        let revision = self.next_revision.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let queued = QueuedLifecycleUpdate { update, revision };
        if let Err(kind) = self.queue.try_send(&self.sender, queued) {
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
    mut receiver: mpsc::Receiver<QueuedLifecycleUpdate>,
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
            while let Some(queued) = receiver.recv().await {
                queue.dequeued();
                if queued.update.parent_session_id() != session_id.0.as_ref() {
                    continue;
                }
                if !ordering.admit(&queued) {
                    // Task ids are client-supplied and unbounded, so the
                    // forwarder logs carry no per-task field; the enclosing
                    // span's bounded session correlation identifies the stream.
                    debug!("Dropped superseded ACP task lifecycle update");
                    continue;
                }
                let task_id = lody_task_id(&queued.update).to_string();
                let previously_emitted = emitted_task_ids.contains(&task_id);
                let send_started_at = Instant::now();
                let send_result = send_task_lifecycle(&client, &session_id, queued.update, previously_emitted);
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
