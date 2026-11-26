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
/// The controller stamps both `Utc::now()` and the lifecycle revision when it
/// records a state change, so two transitions recorded in one tick share
/// `updated_at` and timestamps are not an ordering key. The controller
/// allocates the revision under the state lock that captured the snapshot and
/// delivers it with the entry, which is what keeps a stale capture delivered
/// late from overwriting a newer state. The snapshot pass reserves its base
/// revision from the same controller clock before the observer is registered,
/// so every live capture outranks the initial snapshots. The forwarder orders
/// by that revision and keeps `updated_at` only for the wire contract.
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
/// The controller owns this clock: its `updated_at` is a wall-clock timestamp
/// for the wire contract and two transitions recorded in the same tick share
/// it, so it cannot order them. The controller allocates the revision while it
/// holds the state lock that captured the snapshot, then delivers it with the
/// entry, so revision order and capture order are the same total order across
/// the snapshot pass and the live observer alike. Allocating a revision here,
/// as the item reaches the queue, would instead order deliveries: a stale
/// capture delivered late would take the higher revision and overwrite the
/// newer state it lost to.
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
    /// Starts the session's lifecycle forwarder, if it is not already running.
    ///
    /// The install decision is taken under the session-data lock before either
    /// task is spawned or the observer is registered. Two overlapping
    /// acquisitions of one session can both pass an unlocked fast path, so
    /// spawning first would let the loser publish an initial `ToolCall` — and
    /// its observer forward live updates — before it noticed it had lost. Only
    /// the candidate that wins the install may publish anything.
    pub(super) fn ensure_task_lifecycle_forwarder(&self, session: &SessionHandle) -> anyhow::Result<()> {
        // Resolved before the install lock: both take locks of their own, and
        // neither is part of the install decision.
        let client = self.client();
        let launch_controller = self.local_tool_registry.subagent_controller();
        // Installing the handles is serialized with teardown: both take the
        // session-data lock, so a close cannot stop the lifecycle between
        // spawning these tasks and publishing their handles. The closing state
        // is read under that same lock, so a close that already ran teardown
        // first is observed here and aborts the install instead of leaving a
        // forwarder publishing into a session that has stopped draining it.
        let mut data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("registering ACP task lifecycle forwarder", error))?;
        if session.is_closing() || data.task_lifecycle_forwarder.as_ref().is_some_and(|task| !task.is_finished()) {
            return Ok(());
        }
        let controller = match &data.workspace_runtime {
            Some(_) => data.subagent_controller.clone(),
            None => launch_controller,
        };
        let (Some(controller), Some(client)) = (controller, client) else {
            return Ok(());
        };
        let session_id = data.session_id.clone();
        let metrics = data
            .workspace_runtime
            .as_ref()
            .map(|runtime| runtime.local_tool_registry.metrics_collector())
            .unwrap_or_else(|| self.local_tool_registry.metrics_collector());
        let (sender, receiver) = mpsc::channel(128);
        // The tracker holds the observer as a trait object so tests can
        // substitute a counting double, so the collector Arc is coerced here.
        let queue_metrics = Arc::clone(&metrics) as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
        let queue = Arc::new(LifecycleQueueTracker::new(queue_metrics));
        // Reserve the snapshot pass's revision before the observer exists, so
        // every live transition registered from here takes a strictly newer
        // revision than the snapshots. A snapshot captured from the same state
        // a transition reports must not overwrite the transition. The base
        // comes from the controller's clock, so it is comparable with the
        // revisions the controller stamps on live snapshots.
        let snapshot_revision = controller.reserve_lifecycle_revision();
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
        // Keep the publisher's handle: it writes into the forwarder's queue, so
        // aborting only the forwarder would leave it enqueueing into a queue
        // nothing drains. It is stopped with the forwarder in every path.
        let initial_snapshot_task = tokio::spawn(async move {
            publish_initial_task_snapshots(&controller, &session_id, &sender, &queue, &metrics, snapshot_revision)
                .await;
        });
        if let Some(previous_publisher) = data.task_lifecycle_snapshot_publisher.replace(initial_snapshot_task) {
            previous_publisher.abort();
        }
        data.task_lifecycle_forwarder = Some(forwarder);
        data.task_lifecycle_registration = Some(registration);
        Ok(())
    }
}

struct AcpTaskLifecycleObserver {
    session_id: String,
    sender: mpsc::Sender<QueuedLifecycleUpdate>,
    queue: Arc<LifecycleQueueTracker>,
}

impl SubagentProgressObserver for AcpTaskLifecycleObserver {
    /// Unordered fallback for a caller that delivers without a controller
    /// capture. The controller always publishes through
    /// [`SubagentProgressObserver::subagent_status_published`], so this path
    /// carries revision zero and can never override an ordered capture.
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.subagent_status_published(entry, 0);
    }

    /// Unordered fallback; see [`Self::subagent_status_changed`].
    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.background_status_published(entry, 0);
    }

    fn subagent_status_published(&self, entry: SubagentStatusEntry, lifecycle_revision: u64) {
        self.try_send(
            LifecycleUpdate::Subagent {
                parent_session_id: self.session_id.clone(),
                task: entry,
            },
            lifecycle_revision,
        );
    }

    fn background_status_published(&self, entry: BackgroundSubprocessEntry, lifecycle_revision: u64) {
        self.try_send(
            LifecycleUpdate::BackgroundProcess {
                parent_session_id: self.session_id.clone(),
                task: entry,
            },
            lifecycle_revision,
        );
    }
}

impl AcpTaskLifecycleObserver {
    /// Queues one transition under the revision its capture was stamped with.
    ///
    /// The revision is carried, never allocated here: a capture taken before a
    /// later state change must keep the lower revision even when it reaches
    /// the queue second, or the delayed stale entry would overwrite the newer
    /// state.
    fn try_send(&self, update: LifecycleUpdate, revision: u64) {
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
