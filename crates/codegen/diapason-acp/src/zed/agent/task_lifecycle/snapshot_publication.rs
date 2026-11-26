//! Attach-time publication of the lifecycle snapshots a client has not seen.

use super::queue_accounting::{LifecycleQueueDropKind, LifecycleQueueTracker};
use super::{LifecycleUpdate, QueuedLifecycleUpdate, bounded_session_correlation};
use crate::acp;
use diapason_core::metrics::MetricsCollector;
use diapason_core::subagents::{TaskOwner, owned_child_ids};
use hashbrown::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{Instrument, field, info_span, warn};

/// Queues one snapshot for every task that is still active when the observer
/// is registered.
///
/// Restoration and session attach can publish `Starting`/`Running`
/// transitions before the forwarder exists. Replaying the live snapshot
/// closes that gap without replaying terminal history the client never saw,
/// and because the observer is already registered, any later transition still
/// arrives as a regular update.
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
    revision: u64,
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
    let (published, failure) =
        queue_initial_task_snapshots(controller, session_id, sender, queue, metrics.as_ref(), revision)
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
/// loss: the tracker keeps the newest refused update, and the forwarder
/// delivers it as soon as it has drained a slot, so a task that completes in
/// this window still reaches the client in its terminal state.
///
/// The shared revision ordering still decides what is newest: a snapshot
/// carries the base revision reserved before the observer was registered, so a
/// live capture supersedes it whenever both are delivered, and a refused
/// update replaced by a newer one is dropped rather than reordered.
async fn queue_initial_task_snapshots(
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    sender: &mpsc::Sender<QueuedLifecycleUpdate>,
    queue: &LifecycleQueueTracker,
    metrics: &MetricsCollector,
    revision: u64,
) -> (usize, Option<LifecycleQueueDropKind>) {
    let mut published = 0_usize;
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
        let queued = QueuedLifecycleUpdate { update, revision };
        // This awaited send waits for capacity rather than dropping, so a live
        // terminal update can still be refused while the pass holds a slot; see
        // the function documentation for the residual case.
        match queue.send(sender, queued).await {
            Ok(()) => metrics.record_acp_initial_snapshots_enqueued(1),
            Err(kind) => return (published, Some(kind)),
        }
        published = published.saturating_add(1);
    }

    for controller in &controllers {
        for entry in controller
            .background_status_entries(TaskOwner::Session(session_id.0.as_ref()))
            .await
        {
            let owned = entry.owner_session_id.as_deref() == Some(session_id.0.as_ref());
            if !owned || !entry.status.is_active() {
                continue;
            }
            let update = LifecycleUpdate::BackgroundProcess {
                parent_session_id: session_id.0.to_string(),
                task: entry,
            };
            let queued = QueuedLifecycleUpdate { update, revision };
            match queue.send(sender, queued).await {
                Ok(()) => metrics.record_acp_initial_snapshots_enqueued(1),
                Err(kind) => return (published, Some(kind)),
            }
            published = published.saturating_add(1);
        }
    }
    (published, None)
}
