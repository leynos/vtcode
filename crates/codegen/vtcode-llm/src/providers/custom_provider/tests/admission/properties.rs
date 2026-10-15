use super::super::super::custom_provider_admission_metrics;
use super::{ADMISSION_TEST_LOCK, admission_for_capacity};
use proptest::prelude::*;
use std::future::Future;
use std::task::Poll;
use std::time::Duration;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn generated_admission_operations_preserve_permit_and_queue_invariants(
        capacity in 1_usize..=4,
        operations in prop::collection::vec((0_u8..4, any::<usize>()), 1..32),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build admission model runtime");
        runtime.block_on(async {
            let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
            let provider_name = format!("model-{capacity}");
            let mut admission = admission_for_capacity(&provider_name, capacity);
            admission.queue_timeout = Duration::from_millis(1);
            let metrics_before = custom_provider_admission_metrics();
            let mut held_permits = Vec::with_capacity(capacity);
            let mut expected_waits = 0_u64;
            let mut expected_timeouts = 0_u64;

            for (operation, index) in operations {
                match operation {
                    0 => {
                        expected_waits += 1;
                        if held_permits.len() < capacity {
                            held_permits.push(
                                admission
                                    .acquire()
                                    .await
                                    .expect("available modeled capacity should admit")
                                    .expect("configured capacity should return a permit"),
                            );
                        } else {
                            expected_timeouts += 1;
                            let error = admission.acquire().await.expect_err("full capacity should time out");
                            let crate::provider::LLMError::Provider { metadata, .. } = error else {
                                panic!("full capacity should return a provider error");
                            };
                            prop_assert_eq!(
                                metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
                                Some("custom_provider_admission_timeout"),
                            );
                        }
                    }
                    1 if !held_permits.is_empty() => {
                        let release_index = index % held_permits.len();
                        drop(held_permits.swap_remove(release_index));
                    }
                    2 if held_permits.len() == capacity => {
                        expected_waits += 1;
                        let mut queued_acquisition = Box::pin(admission.acquire());
                        let poll_result = futures::future::poll_fn(|context| {
                            Poll::Ready(queued_acquisition.as_mut().poll(context))
                        })
                        .await;
                        prop_assert!(poll_result.is_pending(), "a full gate must queue acquisition");
                        prop_assert_eq!(
                            custom_provider_admission_metrics().queue_depth,
                            metrics_before.queue_depth + 1,
                        );
                        drop(queued_acquisition);
                        prop_assert_eq!(custom_provider_admission_metrics().queue_depth, metrics_before.queue_depth);
                    }
                    3 if held_permits.len() == capacity => {
                        expected_waits += 1;
                        expected_timeouts += 1;
                        let error = admission.acquire().await.expect_err("full capacity should time out");
                        let crate::provider::LLMError::Provider { metadata, .. } = error else {
                            panic!("full capacity should return a provider error");
                        };
                        prop_assert_eq!(
                            metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
                            Some("custom_provider_admission_timeout"),
                        );
                    }
                    _ => {}
                }

                prop_assert!(held_permits.len() <= capacity, "active permits must stay within capacity");
                prop_assert_eq!(
                    admission.semaphore.as_ref().expect("configured semaphore").available_permits()
                        + held_permits.len(),
                    capacity,
                    "every permit is either held or available",
                );
                prop_assert_eq!(custom_provider_admission_metrics().queue_depth, metrics_before.queue_depth);
                prop_assert_eq!(
                    custom_provider_admission_metrics().in_flight,
                    metrics_before.in_flight + u64::try_from(held_permits.len()).expect("held count fits u64"),
                );
            }

            drop(held_permits);
            prop_assert_eq!(
                admission.semaphore.as_ref().expect("configured semaphore").available_permits(),
                capacity,
                "dropping every active request must return all permits",
            );
            let metrics_after = custom_provider_admission_metrics();
            prop_assert_eq!(metrics_after.queue_depth, metrics_before.queue_depth);
            prop_assert_eq!(metrics_after.in_flight, metrics_before.in_flight);
            prop_assert_eq!(metrics_after.queue_waits - metrics_before.queue_waits, expected_waits);
            prop_assert_eq!(metrics_after.queue_timeouts - metrics_before.queue_timeouts, expected_timeouts);
            Ok(())
        })?;
    }
}
