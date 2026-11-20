//! Resolves and attaches the archive a background session wrote.

use super::*;

use super::controller_background_query::{resolve_background_target, resolve_background_target_mut};

use std::path::Path;

impl SubagentController {
    pub(super) async fn refresh_background_archive_metadata(&self, target: &str) -> Result<()> {
        let (session_id, exec_session_id) = {
            let state = self.state.read().await;
            resolve_background_target(&state, TaskOwner::Workspace, target)
                .map(|record| (record.session_id.clone(), record.exec_session_id.clone()))
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?
        };

        if let Some(listing) = find_session_by_identifier(&session_id).await? {
            let mut state = self.state.write().await;
            attach_background_archive_paths(&mut state, target, &session_id, &exec_session_id, &listing.path);
        }

        Ok(())
    }
}

/// Attaches resolved archive paths to the launch that named the session.
///
/// The lookup ran without the state lock, and a restart can replace the record
/// meanwhile. These paths belong to the launch that named the session the lookup
/// resolved, so they are attached only while the record still names both that
/// session and that launch. The session id alone is not an identity: it is
/// derived from a millisecond timestamp, so two launches of one record in the
/// same millisecond can share it while naming different exec sessions. The
/// exec session identifies the process, so the replacement records its own
/// archive and never inherits the earlier run's.
pub(super) fn attach_background_archive_paths(
    state: &mut ControllerState,
    target: &str,
    session_id: &str,
    exec_session_id: &str,
    archive_path: &Path,
) {
    if let Some(record) = resolve_background_target_mut(state, TaskOwner::Workspace, target)
        && record.session_id == session_id
        && record.exec_session_id == exec_session_id
    {
        record.archive_path = Some(archive_path.to_path_buf());
        record.transcript_path = Some(archive_path.to_path_buf());
    }
}
