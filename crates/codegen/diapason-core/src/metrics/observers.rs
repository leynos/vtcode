//! Observer-port adapters that accumulate service and queue metrics.
//!
//! The collector is the driven adapter for the startup and task-lifecycle
//! notification ports, so both impls live together here.

use super::{
    MetricsCollector, ServiceClientStartupOutcome, ServiceProviderStartupOutcome, ServiceStartupMetricsObserver,
    TaskLifecycleNotificationQueueDrop, TaskLifecycleNotificationQueueMetricsObserver,
};

impl ServiceStartupMetricsObserver for MetricsCollector {
    fn client_startup_started(&self) {
        let mut metrics = self.service_startup.lock();
        metrics.client_attempts_started = metrics.client_attempts_started.saturating_add(1);
        metrics.client_attempts_in_flight = metrics.client_attempts_in_flight.saturating_add(1);
    }

    fn client_startup_finished(
        &self,
        outcome: ServiceClientStartupOutcome,
        previous_active_provider_count: u64,
        active_provider_count: u64,
        duration_ms: u64,
    ) {
        let mut metrics = self.service_startup.lock();
        metrics.client_attempts_in_flight = metrics.client_attempts_in_flight.saturating_sub(1);
        match outcome {
            ServiceClientStartupOutcome::Succeeded => {
                metrics.client_attempts_succeeded = metrics.client_attempts_succeeded.saturating_add(1);
            }
            ServiceClientStartupOutcome::Partial => {
                metrics.client_attempts_partial = metrics.client_attempts_partial.saturating_add(1);
            }
            ServiceClientStartupOutcome::Failed => {
                metrics.client_attempts_failed = metrics.client_attempts_failed.saturating_add(1);
            }
            ServiceClientStartupOutcome::Disabled => {
                metrics.client_attempts_disabled = metrics.client_attempts_disabled.saturating_add(1);
            }
            ServiceClientStartupOutcome::Cancelled => {
                metrics.client_attempts_cancelled = metrics.client_attempts_cancelled.saturating_add(1);
            }
        }
        if active_provider_count >= previous_active_provider_count {
            metrics.active_providers = metrics
                .active_providers
                .saturating_add(active_provider_count.saturating_sub(previous_active_provider_count));
        } else {
            metrics.active_providers = metrics
                .active_providers
                .saturating_sub(previous_active_provider_count.saturating_sub(active_provider_count));
        }
        metrics.total_client_duration_ms = metrics.total_client_duration_ms.saturating_add(duration_ms);
    }

    fn provider_startup_started(&self) {
        let mut metrics = self.service_startup.lock();
        metrics.provider_attempts_started = metrics.provider_attempts_started.saturating_add(1);
        metrics.provider_attempts_in_flight = metrics.provider_attempts_in_flight.saturating_add(1);
    }

    fn provider_startup_finished(&self, outcome: ServiceProviderStartupOutcome, duration_ms: u64) {
        let mut metrics = self.service_startup.lock();
        metrics.provider_attempts_in_flight = metrics.provider_attempts_in_flight.saturating_sub(1);
        match outcome {
            ServiceProviderStartupOutcome::Succeeded => {
                metrics.provider_attempts_succeeded = metrics.provider_attempts_succeeded.saturating_add(1);
            }
            ServiceProviderStartupOutcome::ConnectionFailed => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_connection_failures = metrics.provider_connection_failures.saturating_add(1);
            }
            ServiceProviderStartupOutcome::ConnectionTimedOut => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_connection_timeouts = metrics.provider_connection_timeouts.saturating_add(1);
                metrics.provider_timeouts = metrics.provider_timeouts.saturating_add(1);
            }
            ServiceProviderStartupOutcome::InitializationFailed => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_initialization_failures = metrics.provider_initialization_failures.saturating_add(1);
            }
            ServiceProviderStartupOutcome::InitializationTimedOut => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_initialization_timeouts = metrics.provider_initialization_timeouts.saturating_add(1);
                metrics.provider_timeouts = metrics.provider_timeouts.saturating_add(1);
            }
            ServiceProviderStartupOutcome::OtherFailed => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_other_failures = metrics.provider_other_failures.saturating_add(1);
            }
            ServiceProviderStartupOutcome::Cancelled => {
                metrics.provider_attempts_cancelled = metrics.provider_attempts_cancelled.saturating_add(1);
            }
        }
        metrics.total_provider_duration_ms = metrics.total_provider_duration_ms.saturating_add(duration_ms);
    }
}

impl TaskLifecycleNotificationQueueMetricsObserver for MetricsCollector {
    fn queue_item_enqueued(&self) {
        let mut metrics = self.task_lifecycle_notification_queue.lock();
        metrics.queue_depth = metrics.queue_depth.saturating_add(1);
    }

    fn queue_item_dequeued(&self) {
        let mut metrics = self.task_lifecycle_notification_queue.lock();
        metrics.queue_depth = metrics.queue_depth.saturating_sub(1);
    }

    fn queue_items_drained(&self, entries: usize) {
        let entries = u64::try_from(entries).unwrap_or(u64::MAX);
        let mut metrics = self.task_lifecycle_notification_queue.lock();
        metrics.queue_depth = metrics.queue_depth.saturating_sub(entries);
    }

    fn queue_item_dropped(&self, reason: TaskLifecycleNotificationQueueDrop) -> u64 {
        let mut metrics = self.task_lifecycle_notification_queue.lock();
        match reason {
            TaskLifecycleNotificationQueueDrop::Full => {
                metrics.queue_dropped_full = metrics.queue_dropped_full.saturating_add(1);
                metrics.queue_dropped_full
            }
            TaskLifecycleNotificationQueueDrop::Closed => {
                metrics.queue_dropped_closed = metrics.queue_dropped_closed.saturating_add(1);
                metrics.queue_dropped_closed
            }
        }
    }
}
