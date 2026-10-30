//! Builds standard ACP task updates carrying the negotiated Lody metadata.
use super::super::lody::lody_task_session_update;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use vtcode_core::subagents::{BackgroundSubprocessEntry, SubagentStatusEntry};

/// Observer snapshots queued for one ACP session and projected onto its wire updates.
#[derive(Clone, Debug)]
pub(in crate::zed::agent) enum LifecycleUpdate {
    Subagent {
        parent_session_id: String,
        task: SubagentStatusEntry,
    },
    BackgroundProcess {
        parent_session_id: String,
        task: BackgroundSubprocessEntry,
    },
}

impl LifecycleUpdate {
    pub(super) fn parent_session_id(&self) -> &str {
        match self {
            Self::Subagent { parent_session_id, .. } | Self::BackgroundProcess { parent_session_id, .. } => {
                parent_session_id
            }
        }
    }
}

pub(super) fn send_task_lifecycle(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    event: LifecycleUpdate,
    previously_emitted: bool,
) -> anyhow::Result<()> {
    let update = lody_task_session_update(event, previously_emitted)?;
    client
        .send_session_notification(acp::SessionNotification::new(session_id.clone(), update))
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}
