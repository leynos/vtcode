use crate::error::{Result, WebmcpError};
use diapason_exec_events::VersionedThreadEvent;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub(crate) const MAX_EVENT_BYTES: usize = 512 * 1024;
const MAX_REPLAY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REPLAY_CAPACITY: usize = 4096;
const MAX_SUBSCRIBER_CAPACITY: usize = 1024;
const MAX_SUBSCRIBERS: usize = 1024;
const MAX_SUBSCRIBER_BYTES: usize = 8 * 1024 * 1024;

/// Event hub retention and subscriber queue limits.
#[derive(Debug, Clone, Copy)]
pub struct EventHubConfig {
    /// Number of events retained for reconnect replay.
    pub replay_capacity: usize,
    /// Number of events buffered per connected browser.
    pub subscriber_capacity: usize,
}

impl Default for EventHubConfig {
    fn default() -> Self {
        Self { replay_capacity: 256, subscriber_capacity: 64 }
    }
}

#[derive(Debug, Clone)]
struct HubEvent {
    sequence: u64,
    event: VersionedThreadEvent,
    size_bytes: usize,
}

#[derive(Debug)]
struct HubState {
    next_sequence: u64,
    replay: VecDeque<HubEvent>,
    replay_bytes: usize,
    subscribers: HashMap<u64, Subscriber>,
    next_subscriber_id: u64,
}

#[derive(Debug)]
struct Subscriber {
    sender: mpsc::Sender<HubEvent>,
    queued_bytes: Arc<AtomicUsize>,
}

/// Bounded event hub that retains canonical Diapason runtime events.
#[derive(Clone)]
pub struct WebmcpEventHub {
    config: EventHubConfig,
    max_event_bytes: usize,
    state: Arc<Mutex<HubState>>,
}

/// A sequenced event returned during replay or live subscription.
#[derive(Debug, Clone)]
pub struct SequencedThreadEvent {
    /// Monotonic bridge sequence number.
    pub sequence: u64,
    /// Canonical versioned runtime event.
    pub event: VersionedThreadEvent,
}

/// A browser event subscription with a replay prefix.
pub struct EventHubSubscription {
    replay: Vec<SequencedThreadEvent>,
    receiver: mpsc::Receiver<HubEvent>,
    state: Arc<Mutex<HubState>>,
    subscriber_id: u64,
    queued_bytes: Arc<AtomicUsize>,
}

impl WebmcpEventHub {
    /// Creates a bounded event hub.
    pub fn new(config: EventHubConfig) -> Result<Self> {
        Self::new_with_max_event_bytes(config, MAX_EVENT_BYTES)
    }

    /// Creates a bounded event hub with a smaller event payload limit.
    pub fn new_with_max_event_bytes(config: EventHubConfig, max_event_bytes: usize) -> Result<Self> {
        if config.replay_capacity == 0
            || config.subscriber_capacity == 0
            || config.replay_capacity > MAX_REPLAY_CAPACITY
            || config.subscriber_capacity > MAX_SUBSCRIBER_CAPACITY
            || max_event_bytes == 0
            || max_event_bytes > MAX_EVENT_BYTES
        {
            return Err(WebmcpError::InvalidRequest(
                "event hub capacities are outside the supported bounds".to_string(),
            ));
        }
        Ok(Self {
            config,
            max_event_bytes,
            state: Arc::new(Mutex::new(HubState {
                next_sequence: 1,
                replay: VecDeque::with_capacity(config.replay_capacity),
                replay_bytes: 0,
                subscribers: HashMap::new(),
                next_subscriber_id: 1,
            })),
        })
    }

    /// Publishes a canonical runtime event and returns its bridge sequence.
    pub fn publish(&self, event: VersionedThreadEvent) -> Result<u64> {
        let event_bytes = serde_json::to_vec(&event)?;
        if event_bytes.len() > self.max_event_bytes {
            return Err(WebmcpError::LimitExceeded);
        }
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.checked_add(1).ok_or(WebmcpError::LimitExceeded)?;
        let hub_event = HubEvent { sequence, event, size_bytes: event_bytes.len() };
        state.replay.push_back(hub_event.clone());
        state.replay_bytes = state.replay_bytes.saturating_add(hub_event.size_bytes);
        while state.replay.len() > self.config.replay_capacity || state.replay_bytes > MAX_REPLAY_BYTES {
            if let Some(removed) = state.replay.pop_front() {
                state.replay_bytes = state.replay_bytes.saturating_sub(removed.size_bytes);
            }
        }

        let mut slow_subscribers = Vec::new();
        for (subscriber_id, subscriber) in &state.subscribers {
            let queued_bytes = subscriber.queued_bytes.load(Ordering::Relaxed);
            if queued_bytes.saturating_add(hub_event.size_bytes) > MAX_SUBSCRIBER_BYTES {
                slow_subscribers.push(*subscriber_id);
                continue;
            }
            let _ = subscriber.queued_bytes.fetch_add(hub_event.size_bytes, Ordering::Relaxed);
            if subscriber.sender.try_send(hub_event.clone()).is_err() {
                let _ = subscriber.queued_bytes.fetch_sub(hub_event.size_bytes, Ordering::Relaxed);
                slow_subscribers.push(*subscriber_id);
            }
        }
        for subscriber_id in slow_subscribers {
            drop(state.subscribers.remove(&subscriber_id));
        }
        Ok(sequence)
    }

    /// Subscribes after a sequence, returning retained events before live events.
    pub fn subscribe(&self, after_sequence: Option<u64>) -> Result<EventHubSubscription> {
        let hub_state = Arc::clone(&self.state);
        let mut state = hub_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let replay = match after_sequence {
            None => Vec::new(),
            Some(requested) => {
                if let Some(oldest) = state.replay.front().map(|event| event.sequence)
                    && requested.saturating_add(1) < oldest
                {
                    return Err(WebmcpError::SequenceGap { requested, oldest });
                }
                state
                    .replay
                    .iter()
                    .filter(|event| event.sequence > requested)
                    .map(|event| SequencedThreadEvent {
                        sequence: event.sequence,
                        event: event.event.clone(),
                    })
                    .collect()
            }
        };
        let (sender, receiver) = mpsc::channel(self.config.subscriber_capacity);
        if state.subscribers.len() >= MAX_SUBSCRIBERS {
            return Err(WebmcpError::LimitExceeded);
        }
        let subscriber_id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.checked_add(1).ok_or(WebmcpError::LimitExceeded)?;
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        drop(
            state
                .subscribers
                .insert(subscriber_id, Subscriber { sender, queued_bytes: Arc::clone(&queued_bytes) }),
        );
        drop(state);
        Ok(EventHubSubscription {
            replay,
            receiver,
            state: hub_state,
            subscriber_id,
            queued_bytes,
        })
    }

    /// Returns the latest assigned sequence, or zero before the first event.
    pub fn latest_sequence(&self) -> u64 {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_sequence.saturating_sub(1)
    }
}

impl EventHubSubscription {
    /// Returns retained replay events in sequence order.
    pub fn replay(&self) -> &[SequencedThreadEvent] {
        &self.replay
    }

    /// Waits for the next live event. A closed receiver means the client was
    /// removed because it could not keep up. The subscription owns the state
    /// needed to receive retained and live events after the hub is dropped.
    pub async fn recv(&mut self) -> Option<SequencedThreadEvent> {
        let event = self.receiver.recv().await?;
        let _ = self.queued_bytes.fetch_sub(event.size_bytes, Ordering::Relaxed);
        Some(SequencedThreadEvent { sequence: event.sequence, event: event.event })
    }
}

impl Drop for EventHubSubscription {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(state.subscribers.remove(&self.subscriber_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diapason_exec_events::{ThreadEvent, ThreadStartedEvent, VersionedThreadEvent};

    fn event(id: &str) -> ThreadEvent {
        ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: id.to_string() })
    }

    #[tokio::test]
    async fn replays_events_and_reports_old_sequence_gaps() {
        let hub = WebmcpEventHub::new(EventHubConfig { replay_capacity: 2, subscriber_capacity: 2 }).expect("hub");
        let _ = hub.publish(VersionedThreadEvent::new(event("one"))).expect("publish");
        let _ = hub.publish(VersionedThreadEvent::new(event("two"))).expect("publish");
        let _ = hub.publish(VersionedThreadEvent::new(event("three"))).expect("publish");

        assert!(hub.subscribe(None).expect("fresh subscription").replay().is_empty());
        assert!(matches!(hub.subscribe(Some(0)), Err(WebmcpError::SequenceGap { requested: 0, oldest: 2 })));
        let subscription = hub.subscribe(Some(1)).expect("replay");
        assert_eq!(subscription.replay().len(), 2);
        assert_eq!(subscription.replay()[0].sequence, 2);
    }

    #[tokio::test]
    async fn slow_subscriber_is_closed_instead_of_dropping_silently() {
        let hub = WebmcpEventHub::new(EventHubConfig { replay_capacity: 4, subscriber_capacity: 1 }).expect("hub");
        let mut subscription = hub.subscribe(None).expect("subscription");
        let _ = hub.publish(VersionedThreadEvent::new(event("one"))).expect("publish");
        let _ = hub.publish(VersionedThreadEvent::new(event("two"))).expect("publish");
        assert_eq!(subscription.recv().await.expect("first event").sequence, 1);
        assert!(subscription.recv().await.is_none());
    }

    #[test]
    fn rejects_unbounded_queue_configuration() {
        assert!(
            WebmcpEventHub::new(EventHubConfig {
                replay_capacity: MAX_REPLAY_CAPACITY + 1,
                subscriber_capacity: 1
            })
            .is_err()
        );
        assert!(
            WebmcpEventHub::new(EventHubConfig {
                replay_capacity: 1,
                subscriber_capacity: MAX_SUBSCRIBER_CAPACITY + 1
            })
            .is_err()
        );
    }

    #[test]
    fn rejects_subscriber_overflow() {
        let hub = WebmcpEventHub::new(EventHubConfig::default()).expect("hub");
        let mut subscriptions = Vec::with_capacity(MAX_SUBSCRIBERS);
        for _ in 0..MAX_SUBSCRIBERS {
            subscriptions.push(hub.subscribe(None).expect("subscriber"));
        }
        assert!(matches!(hub.subscribe(None), Err(WebmcpError::LimitExceeded)));
    }
}
