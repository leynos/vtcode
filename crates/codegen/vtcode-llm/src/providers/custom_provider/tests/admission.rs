use super::super::{CustomProviderAdmission, CustomProviderBackendRouter, custom_provider_admission_metrics};
use proptest::prelude::*;
use rstest::rstest;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::Semaphore;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};

static ADMISSION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn admission_router(base_url: &str) -> CustomProviderBackendRouter {
    CustomProviderBackendRouter::from_config(
        CustomProviderConfig {
            name: "shared-admission-regression".to_string(),
            display_name: "Shared Admission Regression".to_string(),
            base_url: base_url.to_string(),
            request_policy: CustomProviderRequestPolicyConfig {
                max_in_flight_requests: Some(1),
                queue_timeout_seconds: 5,
                ..CustomProviderRequestPolicyConfig::default()
            },
            ..CustomProviderConfig::default()
        },
        None,
        None,
        base_url.to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn admission_for_capacity(provider_name: &str, capacity: usize) -> CustomProviderAdmission {
    let config = CustomProviderConfig {
        name: provider_name.to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_in_flight_requests: Some(capacity),
            queue_timeout_seconds: 30,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };

    CustomProviderAdmission::new(&config, &format!("https://{provider_name}.example/v1"))
}

struct AdmissionCapacityWave {
    capacity: usize,
    admission: Arc<CustomProviderAdmission>,
    active_requests: Arc<AtomicUsize>,
    peak_active_requests: Arc<AtomicUsize>,
    started_receiver: tokio::sync::mpsc::Receiver<usize>,
    admitted_receiver: tokio::sync::mpsc::Receiver<usize>,
    release_sender: tokio::sync::watch::Sender<bool>,
    requests: tokio::task::JoinSet<()>,
}

impl AdmissionCapacityWave {
    async fn start(capacity: usize) -> Self {
        let admission = Arc::new(admission_for_capacity(&format!("capacity-{capacity}"), capacity));
        let request_count = capacity * 3;
        let active_requests = Arc::new(AtomicUsize::new(0));
        let peak_active_requests = Arc::new(AtomicUsize::new(0));
        let start_gate = Arc::new(tokio::sync::Barrier::new(request_count + 1));
        let (started_sender, started_receiver) = tokio::sync::mpsc::channel(request_count);
        let (admitted_sender, admitted_receiver) = tokio::sync::mpsc::channel(request_count);
        let (release_sender, release_receiver) = tokio::sync::watch::channel(false);
        let mut requests = tokio::task::JoinSet::new();

        for request_id in 0..request_count {
            let admission = Arc::clone(&admission);
            let active_requests = Arc::clone(&active_requests);
            let peak_active_requests = Arc::clone(&peak_active_requests);
            let start_gate = Arc::clone(&start_gate);
            let started_sender = started_sender.clone();
            let admitted_sender = admitted_sender.clone();
            let mut release_receiver = release_receiver.clone();

            requests.spawn(async move {
                start_gate.wait().await;
                started_sender
                    .send(request_id)
                    .await
                    .expect("test should receive every started request");

                let permit = admission
                    .acquire()
                    .await
                    .expect("request should be admitted")
                    .expect("configured capacity should return a permit");
                let active = active_requests.fetch_add(1, Ordering::SeqCst) + 1;
                peak_active_requests.fetch_max(active, Ordering::SeqCst);
                admitted_sender
                    .send(request_id)
                    .await
                    .expect("test should receive every admitted request");

                release_receiver
                    .wait_for(|released| *released)
                    .await
                    .expect("test should release every admitted request");
                active_requests.fetch_sub(1, Ordering::SeqCst);
                drop(permit);
            });
        }
        drop((started_sender, admitted_sender));
        start_gate.wait().await;

        Self {
            capacity,
            admission,
            active_requests,
            peak_active_requests,
            started_receiver,
            admitted_receiver,
            release_sender,
            requests,
        }
    }

    async fn assert_limit_is_enforced(&mut self) {
        let request_count = self.capacity * 3;
        for _ in 0..request_count {
            tokio::time::timeout(Duration::from_secs(1), self.started_receiver.recv())
                .await
                .expect("all concurrent requests should start")
                .expect("started request channel should remain open");
        }

        let mut admitted_request_ids = Vec::with_capacity(self.capacity);
        for _ in 0..self.capacity {
            admitted_request_ids.push(
                tokio::time::timeout(Duration::from_secs(2), self.admitted_receiver.recv())
                    .await
                    .expect("configured capacity should be admitted")
                    .expect("admitted request channel should remain open"),
            );
        }
        admitted_request_ids.sort_unstable();
        admitted_request_ids.dedup();
        assert_eq!(admitted_request_ids.len(), self.capacity, "each permit admits one request");
        assert_eq!(self.active_requests.load(Ordering::SeqCst), self.capacity);
        assert_eq!(self.peak_active_requests.load(Ordering::SeqCst), self.capacity);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), self.admitted_receiver.recv())
                .await
                .is_err(),
            "no request may be admitted while all configured permits are held"
        );
    }

    async fn release_and_assert_completion(&mut self) {
        self.release_sender.send(true).expect("request workers are still listening");
        while let Some(result) = self.requests.join_next().await {
            result.expect("admission worker should complete without panicking");
        }

        assert_eq!(self.active_requests.load(Ordering::SeqCst), 0);
        assert_eq!(self.peak_active_requests.load(Ordering::SeqCst), self.capacity);
        assert_eq!(
            self.admission
                .semaphore
                .as_ref()
                .expect("configured semaphore")
                .available_permits(),
            self.capacity,
            "completed requests should return every permit"
        );
    }

    async fn assert_permits_are_reusable(&self) {
        let mut reused_permits = Vec::with_capacity(self.capacity);
        for _ in 0..self.capacity {
            reused_permits.push(
                tokio::time::timeout(Duration::from_secs(1), self.admission.acquire())
                    .await
                    .expect("released permit should be reusable")
                    .expect("reused request should be admitted")
                    .expect("configured capacity should return a permit"),
            );
        }
        assert_eq!(
            self.admission
                .semaphore
                .as_ref()
                .expect("configured semaphore")
                .available_permits(),
            0
        );
        drop(reused_permits);
    }
}

#[tokio::test]
async fn router_instances_share_admission_for_the_same_endpoint_and_isolate_other_endpoints() {
    let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
    let first_router = admission_router("https://same.example/v1");
    let second_router = admission_router("https://same.example/v1");
    let other_endpoint = admission_router("https://other.example/v1");
    let first_permit = first_router
        .request_admission
        .acquire()
        .await
        .expect("first router should acquire the configured permit");

    let second_admission = second_router.request_admission.clone();
    let queued = tokio::spawn(async move { second_admission.acquire().await });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished(), "the second router must share the first router's limit");

    let separate_permit = tokio::time::timeout(Duration::from_millis(100), other_endpoint.request_admission.acquire())
        .await
        .expect("a different endpoint should have an independent limit")
        .expect("different endpoint permit");

    drop(first_permit);
    let second_permit = tokio::time::timeout(Duration::from_secs(1), queued)
        .await
        .expect("shared permit should be released")
        .expect("queued task")
        .expect("second router should acquire the released permit");
    drop((second_permit, separate_permit));
}

#[tokio::test]
async fn request_admission_reports_queue_timeout_after_the_configured_wait() {
    let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
    let metrics_before = custom_provider_admission_metrics();
    let admission = CustomProviderAdmission {
        provider_name: Arc::from("queue-timeout-regression"),
        semaphore: Some(Arc::new(Semaphore::new(1))),
        queue_timeout: Duration::from_millis(5),
    };
    let _held = admission.acquire().await.expect("first request permit");

    let error = admission
        .acquire()
        .await
        .expect_err("second request should time out in the queue");

    let crate::provider::LLMError::Provider { message, metadata } = error else {
        panic!("queue timeout should return a provider error");
    };
    assert!(message.contains("configured request limit"));
    assert_eq!(
        metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
        Some("custom_provider_admission_timeout"),
    );
    let metrics_after = custom_provider_admission_metrics();
    assert!(metrics_after.queue_waits > metrics_before.queue_waits);
    assert!(metrics_after.queue_wait_ms > metrics_before.queue_wait_ms);
    assert!(metrics_after.queue_timeouts > metrics_before.queue_timeouts);
    assert!(metrics_after.in_flight >= 1, "the held permit must be included in shared admission metrics");
}

#[tokio::test]
async fn closed_request_admission_has_stable_error_metadata() {
    let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
    let semaphore = Arc::new(Semaphore::new(1));
    semaphore.close();
    let admission = CustomProviderAdmission {
        provider_name: Arc::from("closed-gate-regression"),
        semaphore: Some(semaphore),
        queue_timeout: Duration::from_millis(5),
    };

    let error = admission.acquire().await.expect_err("closed gate should reject admission");

    let crate::provider::LLMError::Provider { metadata, .. } = error else {
        panic!("closed gate should return a provider error");
    };
    assert_eq!(
        metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
        Some("custom_provider_admission_closed"),
    );
}

#[rstest]
#[case(1)]
#[case(2)]
#[case(4)]
#[tokio::test]
async fn request_admission_respects_configured_capacities_and_releases_on_completion(#[case] capacity: usize) {
    let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
    let mut wave = AdmissionCapacityWave::start(capacity).await;
    wave.assert_limit_is_enforced().await;
    wave.release_and_assert_completion().await;
    wave.assert_permits_are_reusable().await;
}

#[tokio::test]
async fn dropping_a_queued_admission_future_does_not_leak_a_permit() {
    let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
    let admission = admission_for_capacity("cancelled-queued-request", 1);
    let held_permit = admission
        .acquire()
        .await
        .expect("first request should be admitted")
        .expect("configured capacity should return a permit");
    let mut queued_acquisition = Box::pin(admission.acquire());
    let poll_result = futures::future::poll_fn(|context| Poll::Ready(queued_acquisition.as_mut().poll(context))).await;

    assert!(poll_result.is_pending(), "acquisition should queue behind the held permit");
    drop(queued_acquisition);
    drop(held_permit);

    let reacquired = tokio::time::timeout(Duration::from_secs(1), admission.acquire())
        .await
        .expect("cancelled waiter should not retain the released permit")
        .expect("permit should remain available after cancellation")
        .expect("configured capacity should return a permit");
    assert_eq!(admission.semaphore.as_ref().expect("configured semaphore").available_permits(), 0);
    drop(reacquired);
    assert_eq!(admission.semaphore.as_ref().expect("configured semaphore").available_permits(), 1);
}

#[path = "admission/properties.rs"]
mod properties;
