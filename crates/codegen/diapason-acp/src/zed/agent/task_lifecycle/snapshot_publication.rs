//! Attach-time publication of the lifecycle snapshots a client has not seen.

use super::queue_accounting::{LifecycleQueueDropKind, LifecycleQueueTracker};
use super::{LifecycleUpdate, QueuedLifecycleUpdate, bounded_session_correlation};
use crate::acp;
use chrono::{DateTime, Utc};
use diapason_core::metrics::MetricsCollector;
use diapason_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentController, SubagentStatus, SubagentStatusEntry,
    TaskOwner, owned_child_ids,
};
use hashbrown::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{Instrument, field, info_span, warn};

/// How many times one snapshot batch is re-read while its state keeps moving.
///
/// The revision between the two reads is only meaningful while they agree. A
/// batch that keeps changing is skipped: every change that made it unstable was
/// itself published by the live observer, so the client is not left without it.
const SNAPSHOT_STABLE_READ_ATTEMPTS: usize = 3;

/// Queues one snapshot for every task that is still active when the observer
/// is registered.
///
/// Restoration and session attach can publish `Starting`/`Running`
/// transitions before the forwarder exists. Replaying the live snapshot
/// closes that gap without replaying terminal history the client never saw,
/// and because the observer is already registered, any later transition still
/// arrives as a regular update.
///
/// # Stamping order
///
/// The controller allocates a live capture's revision under the state lock that
/// read it, so two captures are ordered exactly as the states were read. A
/// snapshot taken at attach time must join that same order: a fixed base
/// revision reserved before the observer was registered would predate the
/// states the pass later read, and a capture that read an earlier state could
/// then be admitted after the snapshot and roll the client back (a `Queued`
/// capture delivered after the snapshot's `Running` leaves the task pending
/// until its next transition).
///
/// Each batch therefore reserves its revision *between* two reads that must
/// agree. A capture that read an earlier state was allocated before that
/// revision and loses to the snapshot; a capture that reads a later state is
/// allocated after it and wins. If the two reads disagree, the state moved
/// while the revision was reserved, so the batch is re-read and stamped again;
/// a batch that never settles within [`SNAPSHOT_STABLE_READ_ATTEMPTS`] is
/// skipped, and the live transitions that kept changing it carry the state.
///
/// This runs unattended in a spawned task, so it owns both its span and its
/// failure accounting: a queue that is already closed leaves the client with
/// no record of the task, and that must be visible in the bounded metrics.
///
/// Each accepted snapshot is counted at the moment its send succeeds, before
/// the next await. Teardown can abort this task while it waits on a later
/// lookup, so counting the whole pass at the end would lose every snapshot it
/// had already published.
pub(super) async fn publish_initial_task_snapshots(
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    sender: &mpsc::Sender<QueuedLifecycleUpdate>,
    queue: &LifecycleQueueTracker,
    metrics: &Arc<MetricsCollector>,
) {
    let session_correlation = bounded_session_correlation(session_id);
    let span = info_span!(
        "acp_task_snapshot_publication",
        session_correlation = %session_correlation,
        outcome = field::Empty,
        elapsed_ms = field::Empty,
        snapshots_published = 0_u64,
        publish_failures = 0_u64
    );
    let started_at = Instant::now();
    let (published, failure) = queue_initial_task_snapshots(controller, session_id, sender, queue, metrics.as_ref())
        .instrument(span.clone())
        .await;
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_ms);
    if published > 0 {
        let _published_recorded = span.record("snapshots_published", u64::try_from(published).unwrap_or(u64::MAX));
    }
    match failure {
        None => {
            let _outcome_recorded = span.record("outcome", "published");
        }
        Some(kind) => {
            let cumulative_count = queue.record_drop(kind);
            metrics.record_acp_initial_snapshot_rejected();
            let _outcome_recorded = span.record("outcome", "queue_closed");
            let _failures_recorded = span.record("publish_failures", 1_u64);
            warn!(
                session_correlation,
                drop_kind = kind.as_str(),
                cumulative_count,
                elapsed_ms,
                "ACP initial task snapshot could not be queued"
            );
        }
    }
}

/// Counts the snapshots this pass queued, plus the first queue rejection.
///
/// # Refused live updates
///
/// This pass waits for room on the same bounded queue live observers use with a
/// non-blocking send. While it waits, and for every snapshot it wins a slot
/// for, a live transition published in the same window can find the queue full
/// and be refused under the `full` queue-drop category. A refusal is not a
/// loss: the tracker keeps the newest refused update for its task, and the
/// forwarder delivers it as soon as it has drained a slot, so a task that
/// completes in this window still reaches the client in its terminal state.
///
/// The shared revision ordering still decides what is newest: a snapshot batch
/// carries the revision reserved between the two reads that confirmed it, so a
/// live capture supersedes it exactly when it read a later state.
async fn queue_initial_task_snapshots(
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    sender: &mpsc::Sender<QueuedLifecycleUpdate>,
    queue: &LifecycleQueueTracker,
    metrics: &MetricsCollector,
) -> (usize, Option<LifecycleQueueDropKind>) {
    let mut published = 0_usize;
    let controllers = snapshot_controllers(controller).await;
    let session_correlation = bounded_session_correlation(session_id);

    match stable_child_snapshot(controller.as_ref(), &controllers).await {
        Some((child_entries, revision)) => {
            let owned_children = owned_child_ids(session_id.0.as_ref(), &child_entries)
                .into_iter()
                .map(str::to_owned)
                .collect::<HashSet<_>>();
            for entry in child_entries {
                if entry.status.is_terminal() || !owned_children.contains(entry.id.as_str()) {
                    continue;
                }
                let queued = QueuedLifecycleUpdate {
                    update: LifecycleUpdate::Subagent {
                        parent_session_id: session_id.0.to_string(),
                        task: entry,
                    },
                    revision,
                };
                // This awaited send waits for capacity rather than dropping, so
                // a live terminal update can still be refused while the pass
                // holds a slot; see the function documentation for the
                // residual case.
                match queue.send(sender, queued).await {
                    Ok(()) => metrics.record_acp_initial_snapshots_enqueued(1),
                    Err(kind) => return (published, Some(kind)),
                }
                published = published.saturating_add(1);
            }
        }
        None => warn!(
            session_correlation,
            snapshot_batch = "children",
            "ACP initial task snapshot batch kept changing while being stamped; live transitions carry its state"
        ),
    }

    match stable_background_snapshot(controller.as_ref(), &controllers, session_id).await {
        Some((background_entries, revision)) => {
            for entry in background_entries {
                let owned = entry.owner_session_id.as_deref() == Some(session_id.0.as_ref());
                if !owned || !entry.status.is_active() {
                    continue;
                }
                let queued = QueuedLifecycleUpdate {
                    update: LifecycleUpdate::BackgroundProcess {
                        parent_session_id: session_id.0.to_string(),
                        task: entry,
                    },
                    revision,
                };
                match queue.send(sender, queued).await {
                    Ok(()) => metrics.record_acp_initial_snapshots_enqueued(1),
                    Err(kind) => return (published, Some(kind)),
                }
                published = published.saturating_add(1);
            }
        }
        None => warn!(
            session_correlation,
            snapshot_batch = "background",
            "ACP initial task snapshot batch kept changing while being stamped; live transitions carry its state"
        ),
    }
    (published, None)
}

/// The controller plus the descendants whose tasks this session can see.
async fn snapshot_controllers(
    controller: &Arc<diapason_core::subagents::SubagentController>,
) -> Vec<Arc<diapason_core::subagents::SubagentController>> {
    let mut controllers = vec![Arc::clone(controller)];
    controllers.extend(controller.descendant_controllers().await);
    controllers
}

/// Reads one child-status batch in a fixed controller order.
async fn read_child_snapshot(
    controllers: &[Arc<diapason_core::subagents::SubagentController>],
) -> Vec<SubagentStatusEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(controller.status_entries().await);
    }
    entries
}

/// Reads one background-status batch for `session_id`.
async fn read_background_snapshot(
    controllers: &[Arc<diapason_core::subagents::SubagentController>],
    session_id: &acp::SessionId,
) -> Vec<BackgroundSubprocessEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(
            controller
                .background_status_entries(TaskOwner::Session(session_id.0.as_ref()))
                .await,
        );
    }
    entries
}

/// The wire-visible state of one child snapshot.
///
/// Every recorded transition stamps `updated_at`, and the status, summary and
/// error are what the client projects, so two reads that agree on these fields
/// describe the same state for ordering purposes. The entry type itself carries
/// no `PartialEq`, and a label or path cannot change without the transition
/// that rewrote the record stamping a new `updated_at`.
type ChildSnapshotKey<'a> = (&'a str, SubagentStatus, DateTime<Utc>, Option<&'a str>, Option<&'a str>);

/// The wire-visible state of one background snapshot.
type BackgroundSnapshotKey<'a> =
    (&'a str, BackgroundSubprocessStatus, DateTime<Utc>, &'a str, bool, Option<&'a str>, Option<&'a str>);

fn child_snapshot_key(entry: &SubagentStatusEntry) -> ChildSnapshotKey<'_> {
    (entry.id.as_str(), entry.status, entry.updated_at, entry.summary.as_deref(), entry.error.as_deref())
}

fn background_snapshot_key(entry: &BackgroundSubprocessEntry) -> BackgroundSnapshotKey<'_> {
    (
        entry.id.as_str(),
        entry.status,
        entry.updated_at,
        entry.exec_session_id.as_str(),
        entry.desired_enabled,
        entry.summary.as_deref(),
        entry.error.as_deref(),
    )
}

/// Whether two child reads describe the same states.
///
/// The reads are compared position by position. Identifiers are unique, so a
/// reordered batch can only produce a false *mismatch* (the state is re-read
/// and stamped again), never a false match: a changed state differs in its own
/// key even if the iteration order moved it. Sorting by id would avoid those
/// extra attempts, but a borrowed sort key outlives the comparison and is not
/// worth the allocation for an attach-time pass.
fn same_child_snapshot(before: &[SubagentStatusEntry], after: &[SubagentStatusEntry]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(before, after)| child_snapshot_key(before) == child_snapshot_key(after))
}

/// Whether two background reads describe the same states.
fn same_background_snapshot(before: &[BackgroundSubprocessEntry], after: &[BackgroundSubprocessEntry]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(before, after)| background_snapshot_key(before) == background_snapshot_key(after))
}

/// Confirms one child batch and stamps it with the revision between its reads.
///
/// Returns `None` when the batch kept moving across every attempt.
async fn stable_child_snapshot(
    clock: &diapason_core::subagents::SubagentController,
    controllers: &[Arc<diapason_core::subagents::SubagentController>],
) -> Option<(Vec<SubagentStatusEntry>, u64)> {
    let mut before = read_child_snapshot(controllers).await;
    for _ in 0..SNAPSHOT_STABLE_READ_ATTEMPTS {
        let revision = clock.reserve_lifecycle_revision();
        let after = read_child_snapshot(controllers).await;
        if same_child_snapshot(&before, &after) {
            return Some((after, revision));
        }
        before = after;
    }
    None
}

/// Confirms one background batch and stamps it with the revision between its
/// reads.
///
/// Returns `None` when the batch kept moving across every attempt.
async fn stable_background_snapshot(
    clock: &diapason_core::subagents::SubagentController,
    controllers: &[Arc<diapason_core::subagents::SubagentController>],
    session_id: &acp::SessionId,
) -> Option<(Vec<BackgroundSubprocessEntry>, u64)> {
    let mut before = read_background_snapshot(controllers, session_id).await;
    for _ in 0..SNAPSHOT_STABLE_READ_ATTEMPTS {
        let revision = clock.reserve_lifecycle_revision();
        let after = read_background_snapshot(controllers, session_id).await;
        if same_background_snapshot(&before, &after) {
            return Some((after, revision));
        }
        before = after;
    }
    None
}
