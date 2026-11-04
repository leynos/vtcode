use anyhow::{Result, anyhow};
use diapason_core::config::constants::tools;
use diapason_ui::tui::app::{InlineHandle, InlineSession};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::agent::runloop::unified::request_user_input;
use crate::agent::runloop::unified::state::CtrlCState;

#[cfg_attr(feature = "profiling", hotpath::measure)]
pub(crate) async fn execute_hitl_tool(
    tool_name: &str,
    handle: &InlineHandle,
    session: &mut InlineSession,
    args: &Value,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    request_user_input_enabled: bool,
) -> Option<Result<Value>> {
    if tool_name == tools::REQUEST_USER_INPUT && !request_user_input_enabled {
        let message = "request_user_input is unavailable in the current runtime";
        return Some(Err(anyhow!(message)));
    }

    match tool_name {
        tools::REQUEST_USER_INPUT => Some(
            request_user_input::execute_request_user_input_tool(handle, session, args, ctrl_c_state, ctrl_c_notify)
                .await,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use diapason_ui::tui::app::{
        InlineCommand, InlineEvent, InlineHandle, InlineListSelection, InlineSession, TransientEvent,
        TransientSubmission,
    };
    use serde_json::json;
    use tokio::sync::{Notify, mpsc};

    use super::execute_hitl_tool;
    use crate::agent::runloop::unified::state::CtrlCState;

    #[tokio::test]
    async fn request_user_input_dispatches_to_inline_wizard_and_returns_choice() {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(command_tx);
        let mut session = InlineSession {
            handle: handle.clone(),
            events: event_rx,
            worker: None,
        };
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let ctrl_c_notify = Arc::new(Notify::new());

        event_tx
            .send(InlineEvent::Transient(TransientEvent::Submitted(TransientSubmission::Wizard(vec![
                InlineListSelection::RequestUserInputAnswer {
                    question_id: "scope".to_string(),
                    selected: vec!["Focused".to_string()],
                    other: None,
                },
            ]))))
            .expect("send wizard answer");

        let result = execute_hitl_tool(
            diapason_core::config::constants::tools::REQUEST_USER_INPUT,
            &handle,
            &mut session,
            &json!({
                "questions": [{
                    "id": "scope",
                    "header": "Scope",
                    "question": "Which scope should the plan cover?",
                    "options": [{
                        "label": "Focused",
                        "description": "Keep the change narrow."
                    }]
                }]
            }),
            &ctrl_c_state,
            &ctrl_c_notify,
            true,
        )
        .await
        .expect("request_user_input should be handled")
        .expect("wizard should return a successful result");

        assert_eq!(result["answers"]["scope"]["selected"], json!(["Focused"]));
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::ShowTransient { request: _ })));
        loop {
            match command_rx.try_recv() {
                Ok(InlineCommand::CloseTransient) => break,
                Ok(_) => {}
                Err(error) => panic!("expected close command, got {error:?}"),
            }
        }
    }
}
