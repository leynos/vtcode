//! Interactive approval for workspace-controlled lifecycle hooks.
//!
//! Workspace-controlled configuration (a workspace-root `config.toml`, the
//! workspace `.diapason/config.toml` fallback, a project profile inside the
//! workspace, or a workspace-sourced agent spec with hooks) can declare
//! lifecycle hook commands that Diapason otherwise runs automatically via
//! `sh -c`. Because that configuration is attacker-influenceable in an
//! untrusted repository, no lifecycle hook runs while workspace-controlled
//! hook content is present until the user approves the exact command set the
//! engine will execute. This module renders that approval prompt and maps the
//! outcome; any cancellation or interrupt fails closed to `Denied`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use diapason_core::core::interfaces::ui::UiSession;
use diapason_core::hooks::LifecycleHookCommandPreview;
use diapason_ui::tui::app::{
    InlineHandle, InlineListItem, InlineListSelection, ListOverlayRequest, TransientRequest, TransientSubmission,
};
use tokio::sync::Notify;

use crate::agent::runloop::unified::overlay_prompt::{OverlayWaitOutcome, show_overlay_and_wait};
use crate::agent::runloop::unified::state::CtrlCState;

pub(crate) enum HookApprovalDecision {
    Approved,
    Denied,
}

/// Present the workspace lifecycle hook approval overlay and wait for the
/// user's choice.
pub(crate) async fn prompt_workspace_hook_approval<S: UiSession + ?Sized>(
    handle: &InlineHandle,
    session: &mut S,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    workspace: &Path,
    commands: &[LifecycleHookCommandPreview],
) -> Result<HookApprovalDecision> {
    let mut lines = vec![
        "The workspace configuration defines lifecycle hook commands that Diapason".to_string(),
        "runs automatically. They execute via `sh -c` from the workspace directory and".to_string(),
        "are controlled by files inside the workspace (config.toml / .diapason), so an".to_string(),
        "untrusted repository could change them. Approving runs the exact command set".to_string(),
        "below; any later change to these commands requires a new approval.".to_string(),
        String::new(),
        format!("Workspace: {}", workspace.display()),
        String::new(),
        "Commands:".to_string(),
    ];
    for command in commands {
        let matcher = command.matcher.as_deref().unwrap_or("*");
        lines.push(format!("  [{event}/{matcher}] {command}", event = command.event, command = command.command));
    }
    lines.push(String::new());
    lines.push("Choose how to handle workspace lifecycle hooks:".to_string());
    lines.push("Use ↑↓ or Tab to navigate • Enter to select • Esc to deny".to_string());

    let items = vec![
        InlineListItem {
            title: "Approve".to_string(),
            subtitle: Some(
                "Run these lifecycle hooks and remember this approval for this exact command set".to_string(),
            ),
            badge: None,
            indent: 0,
            selection: Some(InlineListSelection::ToolApproval(true)),
            search_value: Some("approve allow yes run 1".to_string()),
        },
        InlineListItem {
            title: "Deny".to_string(),
            subtitle: Some("Skip lifecycle hooks for this session".to_string()),
            badge: None,
            indent: 0,
            selection: Some(InlineListSelection::ToolApproval(false)),
            search_value: Some("deny no skip block 2".to_string()),
        },
    ];

    let outcome = show_overlay_and_wait(
        handle,
        session,
        TransientRequest::List(ListOverlayRequest {
            title: "Workspace Lifecycle Hooks".to_string(),
            lines,
            footer_hint: None,
            items,
            selected: Some(InlineListSelection::ToolApproval(false)),
            search: None,
            hotkeys: Vec::new(),
        }),
        ctrl_c_state,
        ctrl_c_notify,
        |submission| match submission {
            TransientSubmission::Selection(InlineListSelection::ToolApproval(true)) => {
                Some(HookApprovalDecision::Approved)
            }
            TransientSubmission::Selection(InlineListSelection::ToolApproval(false)) => {
                Some(HookApprovalDecision::Denied)
            }
            _ => None,
        },
    )
    .await?;

    Ok(match outcome {
        OverlayWaitOutcome::Submitted(decision) => decision,
        OverlayWaitOutcome::Cancelled
        | OverlayWaitOutcome::Interrupted
        | OverlayWaitOutcome::Deferred
        | OverlayWaitOutcome::Exit => HookApprovalDecision::Denied,
    })
}
