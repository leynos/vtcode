//! Executors for the subagent, task, and background-session tools.
//!
//! Each executor decodes the provider's arguments and forwards the call to the
//! shared `SubagentController`, so ownership, admission, and lifecycle policy
//! stay in the core domain rather than being re-derived in the tool layer.
use super::ToolRegistry;
use super::exec_support::sanitize_subagent_tool_output_paths;
use anyhow::{Context, Result, anyhow};
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;

/// Translates the invocation a provider tool runs in into the explicit
/// authority the controller requires.
///
/// The tool executor is an adapter boundary. A tool executes inside the
/// invocation that called it, so its session is the authority it presents; a
/// tool invoked outside any session, such as the local CLI, presents the
/// workspace operator's authority. The controller itself never consults the
/// ambient task-local context.
fn invocation_task_owner(owner_session_id: &Option<String>) -> crate::subagents::TaskOwner<'_> {
    owner_session_id
        .as_deref()
        .map_or(crate::subagents::TaskOwner::Workspace, crate::subagents::TaskOwner::Session)
}

impl ToolRegistry {
    fn require_subagent_controller(&self) -> Result<Arc<crate::subagents::SubagentController>> {
        self.subagent_controller()
            .ok_or_else(|| anyhow!("Subagent controller is not active in this session"))
    }

    fn sanitize_subagent_response(&self, mut value: Value) -> Value {
        sanitize_subagent_tool_output_paths(self.workspace_root(), &mut value);
        value
    }

    async fn execute_subagent_call<Response, F, Fut>(&self, executor: F) -> Result<Value>
    where
        Response: Serialize,
        F: FnOnce(Arc<crate::subagents::SubagentController>) -> Fut,
        Fut: Future<Output = Result<Response>>,
    {
        let controller = self.require_subagent_controller()?;
        let response = executor(controller).await?;
        Ok(self.sanitize_subagent_response(json!(response)))
    }

    async fn execute_subagent_request<Request, Response, F, Fut>(
        &self,
        args: Value,
        parse_context: &'static str,
        executor: F,
    ) -> Result<Value>
    where
        Request: DeserializeOwned,
        Response: Serialize,
        F: FnOnce(Arc<crate::subagents::SubagentController>, Request) -> Fut,
        Fut: Future<Output = Result<Response>>,
    {
        let request = serde_json::from_value::<Request>(args).with_context(|| parse_context.to_string())?;
        self.execute_subagent_call(|controller| executor(controller, request)).await
    }

    /// Unified `agent` executor: dispatches on `action`
    /// (spawn | spawn_subprocess | send_input | resume | wait | close).
    /// For legacy alias calls that omit `action`, the action is inferred from
    /// the argument shape: `id` + `message`/`items` implies send_input, `id`
    /// alone implies resume, otherwise spawn. `wait` and `close` require an
    /// explicit `action` since the schema marks it required.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Legacy agent executor aliases remain available for compatibility tests."
        )
    )]
    pub(crate) fn agent_executor(&self, mut args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let action = crate::subagents::agent_call_action(&args);
            if let Some(obj) = args.as_object_mut() {
                obj.remove("action");
            }
            match action.as_str() {
                "spawn" => self.spawn_agent_executor(args).await,
                "spawn_subprocess" | "spawn_background_subprocess" => {
                    self.spawn_background_subprocess_executor(args).await
                }
                "send_input" => self.send_input_executor(args).await,
                "resume" | "resume_agent" => self.resume_agent_executor(args).await,
                "wait" => self.wait_agent_executor(args).await,
                "close" => self.close_agent_executor(args).await,
                other => Err(anyhow!(
                    "agent: unknown action '{other}'. Use action='spawn' (delegate a task), 'spawn_subprocess' (background daemon), 'send_input' (requires id + message), 'resume' (requires id), 'wait' (requires ids), or 'close' (requires id)."
                )),
            }
        })
    }

    fn spawn_agent_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            self.execute_subagent_request::<crate::subagents::SpawnAgentRequest, _, _, _>(
                args,
                "Invalid spawn_agent arguments",
                |controller, request| async move { controller.spawn(request).await },
            )
            .await
        })
    }

    fn spawn_background_subprocess_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            self.execute_subagent_request::<crate::subagents::SpawnBackgroundSubprocessRequest, _, _, _>(
                args,
                "Invalid spawn_background_subprocess arguments",
                |controller, request| async move { controller.spawn_background_subprocess(request).await },
            )
            .await
        })
    }

    fn send_input_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let owner_session_id = crate::subagents::current_subagent_owner_session_id();
            self.execute_subagent_request::<crate::subagents::SendInputRequest, _, _, _>(
                args,
                "Invalid send_input arguments",
                move |controller, request| async move {
                    let owner = invocation_task_owner(&owner_session_id);
                    controller.send_input(owner, request).await
                },
            )
            .await
        })
    }

    fn wait_agent_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let targets = args
                .get("ids")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("wait_agent requires an ids array"))?
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64);
            let owner_session_id = crate::subagents::current_subagent_owner_session_id();
            self.execute_subagent_call(move |controller| async move {
                let owner = invocation_task_owner(&owner_session_id);
                let entry = controller.wait(owner, &targets, timeout_ms).await?;
                Ok(json!({
                    "completed": entry.is_some(),
                    "entry": entry,
                }))
            })
            .await
        })
    }

    fn resume_agent_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let target = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("resume_agent requires id"))?
                .to_string();
            let owner_session_id = crate::subagents::current_subagent_owner_session_id();
            self.execute_subagent_call(move |controller| async move {
                let owner = invocation_task_owner(&owner_session_id);
                controller.resume(owner, &target).await
            })
            .await
        })
    }

    fn close_agent_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let target = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("close_agent requires id"))?
                .to_string();
            let owner_session_id = crate::subagents::current_subagent_owner_session_id();
            self.execute_subagent_call(move |controller| async move {
                let owner = invocation_task_owner(&owner_session_id);
                controller.close(owner, &target).await
            })
            .await
        })
    }
}

#[cfg(test)]
mod agent_action_dispatch_tests {
    use super::ToolRegistry;
    use serde_json::json;

    /// `agent_executor` must dispatch `action='wait'`/`'close'` to
    /// `wait_agent_executor`/`close_agent_executor`. Both of those validate
    /// their required args (`ids`/`id`) before touching the subagent
    /// controller, so a bare `ToolRegistry::new` (no controller active) still
    /// exercises the routing: the errors below prove the match arm was
    /// reached, not the unknown-action fallback.
    #[tokio::test]
    async fn agent_executor_dispatches_wait_and_close_actions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;

        let wait_err = registry
            .agent_executor(json!({"action": "wait"}))
            .await
            .expect_err("wait without ids should fail");
        let wait_text = wait_err.to_string();
        assert!(!wait_text.contains("unknown action"));
        assert!(wait_text.contains("wait_agent requires an ids array"));

        let close_err = registry
            .agent_executor(json!({"action": "close"}))
            .await
            .expect_err("close without id should fail");
        let close_text = close_err.to_string();
        assert!(!close_text.contains("unknown action"));
        assert!(close_text.contains("close_agent requires id"));
    }

    #[tokio::test]
    async fn agent_executor_rejects_unknown_action_with_guidance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;

        let err = registry
            .agent_executor(json!({"action": "bogus"}))
            .await
            .expect_err("unknown agent action should error");
        let text = err.to_string();
        assert!(text.contains("unknown action 'bogus'"));
        assert!(text.contains("wait"));
        assert!(text.contains("close"));
    }
}
