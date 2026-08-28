//! Builds standard ACP task updates carrying the negotiated Lody metadata.
use super::super::lody::lody_task_session_update;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use vtcode_core::subagents::SubagentProgressEvent;

pub(super) fn send_task_lifecycle(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    event: SubagentProgressEvent,
    previously_emitted: bool,
) -> anyhow::Result<()> {
    let update = lody_task_session_update(event, previously_emitted)?;
    client
        .send_session_notification(acp::SessionNotification::new(session_id.clone(), update))
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}
