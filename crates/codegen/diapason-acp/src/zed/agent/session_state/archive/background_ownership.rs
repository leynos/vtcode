//! Background-ledger ownership reconciliation across an ACP session resume.
//!
//! A resumed session meets two scopes that already hold the workspace ledger:
//! the workspace controller a runtime was built with, and the session
//! controller this attach is about to build. Both are reconciled here, in the
//! order the attach needs and before it restores or registers anything.

use crate::acp;
use crate::zed::agent::ZedAgent;
use crate::zed::agent::log_suppressed_error;
use crate::zed::agent::session_workspace::SessionWorkspaceRuntime;
use crate::zed::session::AcpSessionWorkspace;

impl ZedAgent {
    /// Reconciles the workspace controller with the owner migration this attach
    /// just wrote.
    ///
    /// The workspace runtime can be built before any session resumes — the
    /// launch runtime built by `ZedAgent::new` always is — so its controller may
    /// hold ownerless copies of rows the migration has since given to
    /// `session_id`. Left alone, that controller would restore a row the
    /// session now owns, and its next save would write the ownerless copy back
    /// beside the migrated record, leaving a restorable duplicate of a launch
    /// the session may already have cancelled. The same controller then adopts
    /// the rows it still holds ownerless, which makes the migrated ownership
    /// durable before either scope restores anything.
    ///
    /// It runs before this attach builds the session controller, so the
    /// controller is built from a ledger the workspace controller has already
    /// been given the chance to adopt. Adoption happens whether or not this
    /// session ends up with a controller: the stale copy belongs to the
    /// workspace scope, and leaving it ownerless would let that scope restore a
    /// launch the ledger already attributes to this session.
    ///
    /// Best effort: a failure is logged and the attach continues. Reports
    /// whether the workspace controller's copies carry the owners the ledger
    /// records. `false` means its in-memory view still holds an ownerless copy
    /// of a row this session launched, so the caller must keep the
    /// workspace-scope restore deferred: restoring from that copy would
    /// relaunch work the session owns.
    ///
    /// A save failure does not change the answer. The copies are already
    /// adopted in memory, and the resume-site migration has already written the
    /// ownership to the ledger, so there is nothing left for a restore to
    /// relaunch; the ledger merge also refuses to write an ownerless copy over
    /// an owned record of the same launch.
    pub(super) async fn adopt_workspace_background_owner(
        &self,
        runtime: &SessionWorkspaceRuntime,
        session_id: &acp::SessionId,
    ) -> bool {
        let Some(controller) = runtime.local_tool_registry.subagent_controller() else {
            return true;
        };
        let adopted = match controller.adopt_persisted_background_owner(session_id.0.as_ref()).await {
            Ok(adopted) => adopted,
            Err(error) => {
                log_suppressed_error(&error, "archive_attach", "background_owner_adoption_failed");
                return false;
            }
        };
        if adopted == 0 {
            return true;
        }
        if let Err(error) = controller.save_background_ledger().await {
            log_suppressed_error(&error, "archive_attach", "background_owner_adoption_save_failed");
        }
        true
    }

    /// Gives the ledger rows this resumed session launched to its owner.
    ///
    /// The workspace runtime's restore takes only ownerless rows, so a row this
    /// session launched must carry its owner before that load; otherwise the
    /// workspace controller relaunches it as its own work instead of this
    /// session adopting it.
    ///
    /// Reports whether the migration failed. A failed migration leaves rows
    /// ownerless that this session may yet adopt, so the caller defers the
    /// workspace restore and lets the session controller retry the migration.
    /// Migration is skipped entirely when subagents are disabled.
    pub(super) async fn migrate_resumed_background_owners(
        &self,
        workspace: &AcpSessionWorkspace,
        session_id: &acp::SessionId,
    ) -> bool {
        if !self.vt_config.as_deref().is_some_and(|config| config.subagents.enabled) {
            return false;
        }
        match crate::zed::agent::optional_setup::migrate_resumed_session_background_owners(
            workspace.path(),
            session_id.0.as_ref(),
        )
        .await
        {
            Ok(_) => false,
            Err(error) => {
                log_suppressed_error(&error, "archive_attach", "background_owner_migration_failed");
                true
            }
        }
    }
}
