//! Runs non-local ACP tools through the Zed session protocol.
use super::super::types::SessionCancellation;
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::reports::ToolExecutionReport;
use std::future::Future;
use vtcode_core::llm::provider::ToolCall as ProviderToolCall;

use self::data::ToolCallInvocation;
use super::super::types::{SessionHandle, ToolCallResult};

mod apply_patch;
mod authorization;
mod data;
mod execution;
mod terminal;

pub(super) use self::data::ToolAuditInput;

pub(super) async fn cancelable_session_request<T>(
    cancellation: &SessionCancellation,
    request: impl Future<Output = T>,
) -> Option<T> {
    if cancellation.is_cancelled() {
        return None;
    }

    tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        result = request => Some(result),
    }
}

impl ZedAgent {
    pub(super) async fn execute_tool_calls(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        calls: &[ProviderToolCall],
    ) -> Result<Vec<ToolCallResult>, SdkError> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        let turn_id = uuid::Uuid::new_v4().to_string();
        let Some(client) = self.client() else {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                let report =
                    ToolExecutionReport::failure(Self::tool_name_from_call(call), "Client connection unavailable");
                let result = Self::tool_call_result_from_report(call, report);
                self.record_tool_audit(ToolAuditInput {
                    session,
                    session_id,
                    turn_id: &turn_id,
                    call,
                    effective_arguments: &result.audit_arguments,
                    result: Ok((&result.llm_response, result.audit_status)),
                    duration_ms: 0,
                })
                .await;
                results.push(result);
            }
            return Ok(results);
        };

        let mut results = Vec::with_capacity(calls.len());

        for call in calls {
            self.pace_tool_call(session).await?;
            results.push(
                self.execute_tool_call(ToolCallInvocation {
                    session,
                    session_id,
                    turn_id: &turn_id,
                    call,
                    client: client.as_ref(),
                })
                .await?,
            );
        }

        Ok(results)
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::cancelable_session_request;
    use crate::zed::types::SessionCancellation;
    use std::future::pending;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::oneshot;

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn session_cancellation_drops_the_pending_client_request_future() {
        let cancellation = SessionCancellation::default();
        let waiter_cancellation = cancellation.clone();
        let request_dropped = Arc::new(AtomicBool::new(false));
        let waiter_request_dropped = Arc::clone(&request_dropped);
        let (started_tx, started_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            cancelable_session_request(&waiter_cancellation, async move {
                let _probe = DropProbe(waiter_request_dropped);
                started_tx.send(()).expect("started notification receiver should remain open");
                pending::<()>().await;
            })
            .await
        });

        started_rx.await.expect("pending client request should start");
        cancellation.cancel();
        assert_eq!(task.await.expect("cancellation task should finish"), None);
        assert!(request_dropped.load(Ordering::Acquire), "cancellation must drop the client request future");
    }
}
