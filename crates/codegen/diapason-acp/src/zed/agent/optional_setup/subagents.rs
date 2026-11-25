//! Attachment, owner migration and background restore for ACP sub-agents.

use super::super::log_suppressed_error;
use super::OPTIONAL_SETUP_OPERATION;
use super::background_restore;
use super::outcomes::{OptionalSetupFailure, OptionalSetupOutcome};
use diapason_config::DiapasonConfig;
use diapason_config::core::CustomProviderConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::subagents::{SubagentController, SubagentControllerConfig};
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use futures::future::BoxFuture;
use std::sync::Arc;
use tracing::debug;

pub(in crate::zed::agent) struct AcpSubagentControllerSetup<'a> {
    pub(in crate::zed::agent) owner_session_id: Option<&'a str>,
    pub(in crate::zed::agent) registry: &'a CoreToolRegistry,
    pub(in crate::zed::agent) agent_config: &'a CoreAgentConfig,
    pub(in crate::zed::agent) custom_providers: &'a [CustomProviderConfig],
    pub(in crate::zed::agent) vt_config: Option<&'a DiapasonConfig>,
    pub(in crate::zed::agent) workspace_gated: bool,
    /// Builds the controller without restoring its background sub-agents, so
    /// a caller can run a durability checkpoint before any child is launched.
    ///
    /// It applies to both controller scopes. A resumed session defers its own
    /// restore so a failed attach cannot leave a restored child behind, and a
    /// resumed attach also defers the workspace controller's restore of
    /// ownerless rows: that restore must wait for the same checkpoint, and it
    /// may relaunch a row a session may yet adopt.
    pub(in crate::zed::agent) defer_background_restore: bool,
    /// Whether the resume-site owner migration failed, so controller
    /// construction must retry it before the controller loads the ledger.
    ///
    /// It is independent of `defer_background_restore`: a resumed attach whose
    /// migration succeeded still defers its restore, but must not repeat a
    /// migration that already wrote every ownerless row to its owner.
    pub(in crate::zed::agent) owner_migration_failed: bool,
}

/// Driven boundary for attaching the optional ACP sub-agent controller.
pub(in crate::zed::agent) trait AcpSubagentControllerAttacher: Send + Sync {
    fn attach<'a>(&'a self, setup: AcpSubagentControllerSetup<'a>) -> BoxFuture<'a, OptionalSetupOutcome>;
}

/// Default adapter that attaches the configured ACP sub-agent controller.
pub(in crate::zed::agent) struct DefaultAcpSubagentControllerAttacher;

impl AcpSubagentControllerAttacher for DefaultAcpSubagentControllerAttacher {
    fn attach<'a>(&'a self, setup: AcpSubagentControllerSetup<'a>) -> BoxFuture<'a, OptionalSetupOutcome> {
        attach_acp_subagent_controller(setup)
    }
}

pub(in crate::zed::agent) fn effective_acp_subagent_concurrency(
    configured: usize,
    max_in_flight: Option<usize>,
) -> Option<usize> {
    match max_in_flight {
        Some(0 | 1) => None,
        Some(limit) => Some(configured.min(limit - 1)),
        None => Some(configured),
    }
}

enum AcpSubagentConfiguration {
    DisabledByConfiguration,
    DisabledByPolicy,
    Ready(Box<DiapasonConfig>),
}

/// Attaches the optional ACP sub-agent controller.
///
/// The future is boxed at the source because the background-restore path makes
/// this async body large enough that every caller's `.await` would trip
/// `clippy::large_futures`. Boxing it once here mirrors the
/// [`AcpSubagentControllerAttacher`] and
/// [`super::AcpMcpClientAttacher`] adapters and keeps each caller's
/// future small without changing behaviour.
pub(in crate::zed::agent) fn attach_acp_subagent_controller<'a>(
    setup: AcpSubagentControllerSetup<'a>,
) -> BoxFuture<'a, OptionalSetupOutcome> {
    Box::pin(async move {
        let AcpSubagentControllerSetup {
            owner_session_id,
            registry,
            agent_config,
            custom_providers,
            vt_config,
            workspace_gated,
            defer_background_restore,
            owner_migration_failed,
        } = setup;
        let controller_vt_cfg = match configure_acp_subagents(vt_config, agent_config, custom_providers) {
            AcpSubagentConfiguration::DisabledByConfiguration => {
                return OptionalSetupOutcome::DisabledByConfiguration;
            }
            AcpSubagentConfiguration::DisabledByPolicy => return OptionalSetupOutcome::DisabledByPolicy,
            AcpSubagentConfiguration::Ready(config) => *config,
        };

        let controller_config = SubagentControllerConfig {
            workspace_root: agent_config.workspace.clone(),
            parent_session_id: owner_session_id.unwrap_or("diapason-acp").to_string(),
            parent_model: agent_config.model.clone(),
            parent_provider: agent_config.provider.clone(),
            parent_reasoning_effort: agent_config.reasoning_effort,
            api_key: agent_config.api_key.clone(),
            vt_cfg: controller_vt_cfg.clone(),
            openai_chatgpt_auth: agent_config.openai_chatgpt_auth.clone(),
            depth: 0,
            workspace_gated,
            exec_sessions: registry.exec_session_manager(),
            output_reader: None,
            pty_manager: registry.pty_manager().clone(),
            managed_background_runtime: false,
            lock_scope: None,
        };
        // The controller loads the ledger at construction, so a row this
        // session launched must carry its owner before that load. A resumed
        // attach migrates before building its workspace runtime, and this run
        // is only the retry after that migration failed. A new session's fresh
        // owner can never match a persisted row, so migrating for it would
        // take the ledger's lock for nothing.
        let migration_failed = match owner_session_id {
            Some(owner) if owner_migration_failed => {
                match background_restore::migrate_legacy_owner_rows(&agent_config.workspace, owner).await {
                    Ok(_) => false,
                    Err(error) => {
                        log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "background_owner_migration_failed");
                        true
                    }
                }
            }
            _ => false,
        };
        let controller = match SubagentController::new_with_background_owner(controller_config, owner_session_id).await
        {
            Ok(controller) => Arc::new(controller),
            Err(error) => {
                log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "subagent_controller_unavailable");
                return OptionalSetupOutcome::Failed(OptionalSetupFailure::SubagentControllerUnavailable);
            }
        };
        let mut failures = if defer_background_restore {
            Vec::new()
        } else {
            match owner_session_id {
                Some(_) => {
                    restore_configured_background_subagents(&controller, &controller_vt_cfg, &agent_config.workspace)
                        .await
                }
                // A legacy record has no owner, so no session controller may
                // adopt it. The workspace controller is therefore the only
                // scope that can bring it back, and it restores only those
                // ownerless records.
                None => {
                    restore_legacy_background_subagents(&controller, &controller_vt_cfg, &agent_config.workspace).await
                }
            }
        };
        if migration_failed {
            failures.push(OptionalSetupFailure::BackgroundOwnerMigrationFailed);
        }
        registry.set_subagent_controller(controller);
        OptionalSetupOutcome::attached_with_degradation(failures)
    })
}

/// Migrates ledger rows a resumed ACP session launched to that session's
/// owner.
///
/// A resumed attach runs this before its workspace runtime loads the ledger,
/// because the workspace restore takes ownerless rows. The session controller's
/// own construction retries the migration only when this run failed, because
/// its restore stays deferred until the attach's required checkpoint has
/// succeeded. See [`background_restore::migrate_legacy_owner_rows`].
pub(in crate::zed::agent) async fn migrate_resumed_session_background_owners(
    workspace_root: &std::path::Path,
    owner_session_id: &str,
) -> anyhow::Result<usize> {
    background_restore::migrate_legacy_owner_rows(workspace_root, owner_session_id).await
}

/// Restores the background sub-agents an already-built session controller
/// owns.
///
/// A resumed session defers this until its required history checkpoint has
/// succeeded, because a restored child can outlive a failed attach: the
/// fail-closed path releases the workspace lease, but shutdown aborts only
/// tracked children, never restored background children. A launch whose
/// identity could not be persisted is rolled back instead, so a retried attach
/// cannot start a second process for the same record.
pub(in crate::zed::agent) async fn restore_acp_session_background_subagents(
    controller: &SubagentController,
    vt_config: Option<&DiapasonConfig>,
    workspace_root: &std::path::Path,
) -> OptionalSetupOutcome {
    let Some(config) = vt_config.filter(|config| config.subagents.enabled) else {
        return OptionalSetupOutcome::DisabledByConfiguration;
    };
    let failures = restore_configured_background_subagents(controller, config, workspace_root).await;
    OptionalSetupOutcome::attached_with_degradation(failures)
}

/// Restores the ownerless background records an already-built workspace
/// controller holds.
///
/// A workspace runtime built for a resumed attach skipped this restore, because
/// the rows that session launched may still be ownerless and would have been
/// relaunched as workspace work, and because a restored child must not outlive
/// an attach that later fails. Once a resume has reconciled ownership and
/// passed its required checkpoint, the skip is retried through this entry
/// point, which is the same legacy-only restore the build would have run. The
/// caller keeps the obligation when the pass fails, so a later acquisition can
/// retry it.
pub(in crate::zed::agent) async fn restore_workspace_background_subagents(
    controller: &SubagentController,
    config: &DiapasonConfig,
    workspace_root: &std::path::Path,
) -> OptionalSetupOutcome {
    OptionalSetupOutcome::attached_with_degradation(
        restore_legacy_background_subagents(controller, config, workspace_root).await,
    )
}

fn configure_acp_subagents(
    vt_cfg: Option<&DiapasonConfig>,
    config: &CoreAgentConfig,
    custom_providers: &[CustomProviderConfig],
) -> AcpSubagentConfiguration {
    let Some(mut controller_vt_cfg) = vt_cfg.filter(|config| config.subagents.enabled).cloned() else {
        return AcpSubagentConfiguration::DisabledByConfiguration;
    };
    let max_in_flight = custom_providers
        .iter()
        .find(|provider| provider.name.eq_ignore_ascii_case(&config.provider))
        .and_then(|provider| provider.request_policy.max_in_flight_requests);
    let Some(max_in_flight) = max_in_flight else {
        return AcpSubagentConfiguration::Ready(Box::new(controller_vt_cfg));
    };
    let Some(max_subagents) =
        effective_acp_subagent_concurrency(controller_vt_cfg.subagents.max_concurrent, Some(max_in_flight))
    else {
        return AcpSubagentConfiguration::DisabledByPolicy;
    };
    if controller_vt_cfg.subagents.max_concurrent > max_subagents {
        debug!(
            provider = %config.provider,
            max_in_flight,
            configured_subagents = controller_vt_cfg.subagents.max_concurrent,
            effective_subagents = max_subagents,
            "Capping ACP subagent concurrency to preserve provider request capacity"
        );
        controller_vt_cfg.subagents.max_concurrent = max_subagents;
    }
    AcpSubagentConfiguration::Ready(Box::new(controller_vt_cfg))
}

async fn restore_configured_background_subagents(
    controller: &SubagentController,
    config: &DiapasonConfig,
    workspace_root: &std::path::Path,
) -> Vec<OptionalSetupFailure> {
    if !config.subagents.background.auto_restore {
        return Vec::new();
    }
    match background_restore::restore_owned(controller, workspace_root).await {
        Ok(_) => Vec::new(),
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "background_subagent_restore_failed");
            vec![OptionalSetupFailure::BackgroundSubagentRestoreFailed]
        }
    }
}

/// Restores background records that predate session ownership.
///
/// A session controller must not adopt an ownerless record, so the workspace
/// controller is the only scope that can restore it. Only ownerless records
/// are restored, so a record another session owns is never respawned here.
async fn restore_legacy_background_subagents(
    controller: &SubagentController,
    config: &DiapasonConfig,
    workspace_root: &std::path::Path,
) -> Vec<OptionalSetupFailure> {
    if !config.subagents.background.auto_restore {
        return Vec::new();
    }
    match background_restore::restore_legacy(controller, workspace_root).await {
        Ok(_) => Vec::new(),
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "legacy_background_subagent_restore_failed");
            vec![OptionalSetupFailure::BackgroundSubagentRestoreFailed]
        }
    }
}
