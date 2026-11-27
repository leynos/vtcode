//! Queue accounting for ACP task-lifecycle notifications.
//!
//! The forwarder drains a bounded channel, and every publication path must
//! count under the same lock that makes the item observable so the reported
//! depth never drifts from the channel's contents.
//!
//! A refusal is parked per task rather than in one slot: revisions only order
//! one task's states, so a single slot would let a later refusal for one task
//! evict another task's terminal state and leave its last delivered snapshot
//! active forever. The per-task map is bounded, and a refusal that would exceed
//! the bound is reported rather than silently displacing another task.

use super::QueuedLifecycleUpdate;
use diapason_core::metrics::{TaskLifecycleNotificationQueueDrop, TaskLifecycleNotificationQueueMetricsObserver};
use hashbrown::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

/// The most distinct tasks whose refused updates one tracker keeps.
///
/// The parked map coalesces by task, so its natural size is the number of
/// tasks a refusal can cover before the forwarder drains a slot. The bound is
/// what keeps a stream of refusals from retaining every task a client ever
/// named; when it is reached the pass records the loss explicitly instead of
/// displacing another task's terminal state without a trace.
pub(super) const PARKED_TASK_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LifecycleQueueDropKind {
    Full,
    Closed,
}

impl LifecycleQueueDropKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Closed => "closed",
        }
    }
}

#[derive(Default)]
pub(super) struct LifecycleQueueState {
    depth: usize,
    closed: bool,
    /// The newest update a full queue refused for each task, kept for later
    /// delivery.
    ///
    /// Keying by task is what keeps one task's refusal from evicting another
    /// task's terminal state. Revisions are only comparable per task, so a
    /// newer revision of task B says nothing about the state of task A; a
    /// single slot would discard A's completion for B's next progress update
    /// and leave the client with A's queued active snapshot and no terminal
    /// update to correct it.
    parked: HashMap<String, QueuedLifecycleUpdate>,
}

impl LifecycleQueueState {
    /// Keeps the newest refused update for its task so the forwarder can
    /// deliver it later.
    ///
    /// Revisions are strictly increasing per task, so a later refusal of the
    /// same task replaces the parked one; an older revision is superseded and
    /// would be dropped on delivery anyway. A refusal for a different task
    /// keeps its own entry, so every affected task is recovered when the
    /// forwarder drains.
    ///
    /// The map is bounded by [`PARKED_TASK_LIMIT`]. When a new task would
    /// exceed it, the parked update with the lowest revision is evicted and
    /// reported, because the capacity cannot honour every task: the client's
    /// documented recovery path (`_lody/subagents/list`) is the explicit
    /// resynchronization for the evicted state.
    fn park(&mut self, update: QueuedLifecycleUpdate) -> Option<String> {
        let task_id = update.task_id().to_owned();
        if let Some(parked) = self.parked.get_mut(&task_id) {
            if update.revision >= parked.revision {
                *parked = update;
            }
            return None;
        }
        let evicted = if self.parked.len() >= PARKED_TASK_LIMIT {
            self.evict_least_current()
        } else {
            None
        };
        let _replaced = self.parked.insert(task_id, update);
        evicted
    }

    /// Removes the parked update with the lowest revision.
    ///
    /// The lowest revision is the least current state in the map, so it is
    /// the one a later snapshot or transition is most likely to supersede.
    fn evict_least_current(&mut self) -> Option<String> {
        let victim = self
            .parked
            .iter()
            .min_by_key(|(_, parked)| parked.revision)
            .map(|(task_id, _)| task_id.clone())?;
        let _evicted = self.parked.remove(&victim);
        Some(victim)
    }
}

pub(super) struct LifecycleQueueTracker {
    pub(super) state: Mutex<LifecycleQueueState>,
    metrics: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>,
}

impl LifecycleQueueTracker {
    pub(super) fn new(metrics: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>) -> Self {
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
    pub(super) fn record_enqueued_locked(&self, state: &mut LifecycleQueueState) {
        state.depth = state.depth.saturating_add(1);
        self.metrics.queue_item_enqueued();
    }

    /// Enqueues without waiting, parking the update when the queue is full.
    ///
    /// A full queue refuses the live update, but the refusal must not lose a
    /// task's terminal state: the newest refused update is kept for its task on
    /// the tracker and [`Self::next_update`] delivers it once the forwarder has
    /// drained a slot. The refusal is still counted, because the queue did
    /// refuse it.
    pub(super) fn try_send(
        &self,
        sender: &mpsc::Sender<QueuedLifecycleUpdate>,
        update: QueuedLifecycleUpdate,
    ) -> Result<(), LifecycleQueueDropKind> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(LifecycleQueueDropKind::Closed);
        }
        match sender.try_send(update) {
            Ok(()) => {
                self.record_enqueued_locked(&mut state);
                Ok(())
            }
            Err(TrySendError::Full(update)) => {
                let evicted = state.park(update);
                drop(state);
                if let Some(task_id) = evicted {
                    // The parked map is bounded, so a refusal can displace a
                    // state the client has not seen. Report that explicitly:
                    // `_lody/subagents/list` is the documented resynchronization
                    // for a client whose task state was lost this way.
                    let cumulative_count = self.metrics.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Full);
                    tracing::warn!(
                        queue = "acp_task_lifecycle",
                        drop_kind = LifecycleQueueDropKind::Full.as_str(),
                        cumulative_count,
                        parked_task_limit = PARKED_TASK_LIMIT,
                        task_id = task_id.as_str(),
                        "Evicted a parked ACP task lifecycle update to stay within its bound"
                    );
                }
                Err(LifecycleQueueDropKind::Full)
            }
            Err(TrySendError::Closed(_)) => Err(LifecycleQueueDropKind::Closed),
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
    pub(super) async fn send(
        &self,
        sender: &mpsc::Sender<QueuedLifecycleUpdate>,
        update: QueuedLifecycleUpdate,
    ) -> Result<(), LifecycleQueueDropKind> {
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

    /// Returns the next update to forward, preferring one the queue refused.
    ///
    /// A parked update was never in the channel, so only a channel item
    /// decrements the depth. A refusal can only happen while the channel is
    /// full, which leaves a later receive pending: the forwarder therefore
    /// always visits this method again and cannot strand what it parked.
    pub(super) async fn next_update(
        &self,
        receiver: &mut mpsc::Receiver<QueuedLifecycleUpdate>,
    ) -> Option<QueuedLifecycleUpdate> {
        if let Some(parked) = self.take_parked() {
            return Some(parked);
        }
        let queued = receiver.recv().await?;
        self.dequeued();
        Some(queued)
    }

    /// Takes the parked update with the lowest revision, if one is waiting.
    ///
    /// Delivering the oldest first keeps the forwarder's per-task comparison
    /// monotonic: every later entry in this map carries a higher revision for
    /// its own task, and revisions are only compared per task.
    fn take_parked(&self) -> Option<QueuedLifecycleUpdate> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let task_id = state
            .parked
            .iter()
            .min_by_key(|(_, parked)| parked.revision)
            .map(|(task_id, _)| task_id.clone())?;
        state.parked.remove(&task_id)
    }

    #[cfg(test)]
    pub(super) fn depth(&self) -> usize {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).depth
    }

    pub(super) fn dequeued(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.depth > 0 {
            state.depth -= 1;
            self.metrics.queue_item_dequeued();
        }
    }

    pub(super) fn record_drop(&self, kind: LifecycleQueueDropKind) -> u64 {
        match kind {
            LifecycleQueueDropKind::Full => self.metrics.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Full),
            LifecycleQueueDropKind::Closed => {
                self.metrics.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Closed)
            }
        }
    }

    pub(super) fn close_and_drain(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        // The forwarder has stopped, so a parked update has no reader left;
        // dropping them keeps a closed tracker from retaining task state.
        state.parked.clear();
        let remaining = std::mem::take(&mut state.depth);
        if remaining > 0 {
            self.metrics.queue_items_drained(remaining);
        }
    }
}

pub(super) struct LifecycleQueueForwarderGuard(pub(super) Arc<LifecycleQueueTracker>);

impl Drop for LifecycleQueueForwarderGuard {
    fn drop(&mut self) {
        self.0.close_and_drain();
    }
}
