//! Regression coverage for lifecycle queueing, mapping, and ACP forwarding.

use super::*;
use std::fmt::Write as FmtWrite;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};
use vtcode_core::metrics::MetricsCollector;

#[derive(Clone, Default)]
struct LifecycleTraceCapture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for LifecycleTraceCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, _id: &tracing::Id, _context: Context<'_, S>) {
        if attrs.metadata().name() != "acp_task_lifecycle_forwarder" {
            return;
        }
        let mut fields = CapturedFields::default();
        attrs.record(&mut fields);
        self.0
            .lock()
            .expect("trace capture lock")
            .push(format!("span={}{}", attrs.metadata().name(), fields.0));
    }

    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        if event.metadata().target() != "vtcode_acp::zed::agent::task_lifecycle" {
            return;
        }
        let Some(span) = context.lookup_current() else { return };
        if span.name() != "acp_task_lifecycle_forwarder" {
            return;
        }
        let mut fields = CapturedFields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .expect("trace capture lock")
            .push(format!("event={}{}", event.metadata().name(), fields.0));
    }
}

#[derive(Default)]
struct CapturedFields(String);

impl Visit for CapturedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }
}

#[test]
fn classifies_lifecycle_queue_full_and_closed_errors() {
    let (sender, _receiver) = mpsc::channel::<()>(1);
    sender.try_send(()).expect("first send fits in the queue");
    let full_error = sender.try_send(()).expect_err("second send should find a full queue");
    assert_eq!(lifecycle_queue_drop_kind(full_error), LifecycleQueueDropKind::Full);

    let (sender, receiver) = mpsc::channel::<()>(1);
    drop(receiver);
    let closed_error = sender.try_send(()).expect_err("closed receiver should reject a send");
    assert_eq!(lifecycle_queue_drop_kind(closed_error), LifecycleQueueDropKind::Closed);
}

#[test]
fn lifecycle_queue_metrics_track_depth_and_bounded_drop_categories() {
    let metrics = Arc::new(MetricsCollector::new());
    let observer: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver> = metrics.clone();
    let queue = LifecycleQueueTracker::new(observer);
    let (sender, mut receiver) = mpsc::channel(1);

    queue.try_send(&sender, ()).expect("first queue entry should fit");
    assert_eq!(metrics.get_task_lifecycle_notification_queue_metrics().queue_depth, 1);
    let full = queue
        .try_send(&sender, ())
        .expect_err("second queue entry should find a full queue");
    assert_eq!(full, LifecycleQueueDropKind::Full);
    assert_eq!(queue.record_drop(full), 1);

    receiver.try_recv().expect("first queue entry should be available");
    queue.dequeued();
    assert_eq!(metrics.get_task_lifecycle_notification_queue_metrics().queue_depth, 0);

    drop(receiver);
    let closed = queue.try_send(&sender, ()).expect_err("closed queue should reject an entry");
    assert_eq!(closed, LifecycleQueueDropKind::Closed);
    assert_eq!(queue.record_drop(closed), 1);

    assert_eq!(metrics.get_task_lifecycle_notification_queue_metrics().queue_dropped_full, 1);
    assert_eq!(metrics.get_task_lifecycle_notification_queue_metrics().queue_dropped_closed, 1);
}

#[test]
fn lifecycle_forwarder_shutdown_drains_queued_depth_before_closing_tracker() {
    let metrics = Arc::new(MetricsCollector::new());
    let observer: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver> = metrics.clone();
    let queue = LifecycleQueueTracker::new(observer);
    let (sender, _receiver) = mpsc::channel(1);
    queue.try_send(&sender, ()).expect("queue entry should fit");
    assert_eq!(metrics.get_task_lifecycle_notification_queue_metrics().queue_depth, 1);

    queue.close_and_drain();

    assert_eq!(metrics.get_task_lifecycle_notification_queue_metrics().queue_depth, 0);
    assert_eq!(
        queue
            .try_send(&sender, ())
            .expect_err("closed tracker should reject new entries"),
        LifecycleQueueDropKind::Closed
    );
}

#[test]
fn lifecycle_forwarder_outcome_is_bounded_by_send_result() {
    assert_eq!(task_lifecycle_forwarder_outcome(0), "completed");
    assert_eq!(task_lifecycle_forwarder_outcome(1), "send_failed");
}

/// Reference model for [`LifecycleQueueTracker`] depth accounting.
///
/// Depth only grows on a successful enqueue, only shrinks on a dequeue of a
/// live entry, is never driven negative by duplicate dequeues, and is forced
/// to zero — then held there — by closure. The generated sequences are
/// sequential and assert bookkeeping conservation, not concurrency safety.
#[derive(Debug, Default)]
struct QueueDepthModel {
    depth: usize,
    closed: bool,
}

#[derive(Clone, Copy, Debug)]
enum QueueOp {
    Enqueue,
    Dequeue,
    Close,
}

#[test]
fn lifecycle_queue_depth_matches_the_reference_model() {
    use proptest::prelude::*;

    let strategy = prop::collection::vec(
        prop_oneof![Just(QueueOp::Enqueue), Just(QueueOp::Dequeue), Just(QueueOp::Close),],
        0..16,
    );

    proptest!(ProptestConfig { cases: 128, max_shrink_iters: 256, ..ProptestConfig::default() },
        |(ops in strategy)| {
            let metrics = Arc::new(MetricsCollector::new());
            let observer: Arc<dyn TaskLifecycleNotificationQueueMetricsObserver> = metrics.clone();
            let queue = LifecycleQueueTracker::new(observer);
            // Capacity 2 keeps "full" reachable inside short sequences.
            let (sender, mut receiver) = mpsc::channel(2);
            let mut model = QueueDepthModel::default();

            for op in ops {
                match op {
                    QueueOp::Enqueue => {
                        let result = queue.try_send(&sender, ());
                        match result {
                            Ok(()) => {
                                prop_assert!(!model.closed, "a closed tracker must reject entries");
                                prop_assert!(model.depth < 2, "a full channel must reject entries");
                                model.depth += 1;
                            }
                            Err(kind) => {
                                // A rejected enqueue never changes depth.
                                if kind == LifecycleQueueDropKind::Closed {
                                    prop_assert!(
                                        model.closed || model.depth >= 2,
                                        "a closed drop requires a closed tracker or a full queue"
                                    );
                                } else {
                                    prop_assert!(model.closed || model.depth == 2, "Full requires a full queue");
                                }
                            }
                        }
                    }
                    QueueOp::Dequeue => {
                        // Only dequeue when a real entry exists.
                        if model.depth > 0 && !model.closed {
                            let entry = receiver.try_recv();
                            prop_assert!(entry.is_ok(), "a live depth implies a real queued entry");
                            queue.dequeued();
                            model.depth -= 1;
                        } else {
                            // A spurious dequeue must not drive depth negative.
                            queue.dequeued();
                        }
                    }
                    QueueOp::Close => {
                        queue.close_and_drain();
                        model.depth = 0;
                        model.closed = true;
                    }
                }

                let observed = metrics
                    .get_task_lifecycle_notification_queue_metrics()
                    .queue_depth;
                prop_assert_eq!(observed, model.depth as u64);
            }

            // Closure is terminal.
            if model.closed {
                prop_assert_eq!(
                    queue.try_send(&sender, ()).expect_err("closed tracker must reject entries"),
                    LifecycleQueueDropKind::Closed
                );
                queue.close_and_drain();
                prop_assert_eq!(
                    metrics.get_task_lifecycle_notification_queue_metrics().queue_depth,
                    0
                );
            }
            drop(receiver);
        }
    );
}

#[test]
fn session_correlation_is_stable_bounded_and_does_not_expose_the_session_id() {
    let session_id = acp::SessionId::new(Arc::from("sensitive-parent-session-id"));
    let other_session_id = acp::SessionId::new(Arc::from("another-parent-session-id"));

    let correlation = bounded_session_correlation(&session_id);

    assert_eq!(correlation.len(), SESSION_CORRELATION_HEX_LENGTH);
    assert!(correlation.chars().all(|character| character.is_ascii_hexdigit()));
    assert_eq!(correlation, bounded_session_correlation(&session_id));
    assert_ne!(correlation, bounded_session_correlation(&other_session_id));
    assert!(!correlation.contains("sensitive-parent-session-id"));
}

#[test]
fn notification_failure_warning_includes_bounded_correlation_category_and_latency() {
    let trace_capture = LifecycleTraceCapture::default();
    let subscriber = Registry::default().with(trace_capture.clone());
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let session_id = acp::SessionId::new(Arc::from("sensitive-parent-session-id"));
    let correlation = bounded_session_correlation(&session_id);
    let span = tracing::info_span!(
        "acp_task_lifecycle_forwarder",
        session_correlation = %correlation,
        outcome = field::Empty,
        elapsed_ms = field::Empty,
        notifications_sent = 0_u64,
        send_failures = 0_u64
    );
    let _entered = span.enter();

    warn_task_lifecycle_notification_failure(&correlation, 17);

    let captured = trace_capture.0.lock().expect("trace capture lock").join("\n");
    let warning = captured
        .lines()
        .find(|line| line.contains("Failed to forward ACP task lifecycle notification"))
        .expect("notification failure warning");
    assert!(warning.contains(&correlation));
    assert!(warning.contains("failure_category=\"notification_send_failed\""));
    assert!(warning.contains("notification_send_elapsed_ms=17"));
    assert!(!warning.contains("sensitive-parent-session-id"));
}
