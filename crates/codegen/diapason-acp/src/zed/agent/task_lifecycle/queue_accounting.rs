//! Queue accounting for ACP task-lifecycle notifications.
//!
//! The forwarder drains a bounded channel, and every publication path must
//! count under the same lock that makes the item observable so the reported
//! depth never drifts from the channel's contents.

use super::QueuedLifecycleUpdate;
use diapason_core::metrics::{TaskLifecycleNotificationQueueDrop, TaskLifecycleNotificationQueueMetricsObserver};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

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
    /// The newest update a full queue refused, kept for later delivery.
    parked: Option<QueuedLifecycleUpdate>,
}

impl LifecycleQueueState {
    /// Keeps the newest refused update so the forwarder can deliver it later.
    ///
    /// One slot bounds what a stream of refusals can retain. Revisions are
    /// strictly increasing, so the newest is the one worth keeping: an older
    /// refused update is superseded by whatever replaced it, and the revision
    /// ordering would drop it on delivery anyway.
    fn park(&mut self, update: QueuedLifecycleUpdate) {
        let supersedes = match &self.parked {
            None => true,
            Some(parked) => update.revision >= parked.revision,
        };
        if supersedes {
            self.parked = Some(update);
        }
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
    /// task's terminal state: the newest refused update is kept on the tracker
    /// and [`Self::next_update`] delivers it once the forwarder has drained a
    /// slot. The refusal is still counted, because the queue did refuse it.
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
                state.park(update);
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

    /// Takes the newest update a full queue refused, if one is waiting.
    fn take_parked(&self) -> Option<QueuedLifecycleUpdate> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.parked.take()
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
        // dropping it keeps a closed tracker from retaining task state.
        state.parked = None;
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
