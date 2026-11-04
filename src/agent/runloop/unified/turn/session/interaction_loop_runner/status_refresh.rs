use std::path::Path;

use super::super::interaction_loop::{InteractionLoopContext, InteractionState};
use super::support::{refresh_live_ide_context_update, selected_model_supports_image_input};
use crate::agent::runloop::unified::context_manager::ContextManager;
use crate::agent::runloop::unified::session_setup::{IdeContextBridge, apply_ide_context_snapshot};
use crate::agent::runloop::unified::state::SessionStats;
use crate::agent::runloop::unified::status_line;
use crate::agent::runloop::unified::turn::turn_processing::resolve_effective_request_model;
use diapason_core::config::loader::DiapasonConfig;
use diapason_core::llm::provider::LLMProvider;
use diapason_core::tools::registry::ToolRegistry;
use diapason_ui::tui::app::{InlineHandle, InlineHeaderContext};

#[derive(Clone, Copy)]
pub(super) enum StatusRefreshReason {
    Cadence,
    ConfigurationReloaded,
}

impl StatusRefreshReason {
    fn configuration_reloaded(self) -> bool {
        matches!(self, Self::ConfigurationReloaded)
    }
}

#[derive(Clone, Copy)]
pub(super) struct StatusRefreshRequest {
    pub(super) reason: StatusRefreshReason,
}

/// The smallest set of runtime state needed by the status/IDE/title refresh.
/// Keeping this adapter separate from `InteractionLoopContext` prevents this
/// UI cadence from gaining accidental access to turn execution state.
pub(super) struct StatusRefreshContext<'a> {
    handle: &'a InlineHandle,
    header_context: &'a mut InlineHeaderContext,
    ide_context_bridge: &'a mut Option<IdeContextBridge>,
    context_manager: &'a mut ContextManager,
    active_primary_agent: &'a diapason_core::primary_agent::ActivePrimaryAgentState,
    workspace: &'a Path,
    provider: &'a str,
    model: &'a str,
    reasoning_effort: &'a str,
    vt_cfg: Option<&'a DiapasonConfig>,
    provider_client: &'a dyn LLMProvider,
    tool_registry: &'a ToolRegistry,
    active_thread_label: &'a str,
    session_stats: &'a SessionStats,
}

impl<'a> StatusRefreshContext<'a> {
    pub(super) fn from_loop(ctx: &'a mut InteractionLoopContext<'_>) -> Self {
        Self {
            handle: ctx.handle,
            header_context: ctx.header_context,
            ide_context_bridge: ctx.ide_context_bridge,
            context_manager: ctx.context_manager,
            active_primary_agent: &*ctx.active_primary_agent,
            workspace: ctx.config.workspace.as_path(),
            provider: &ctx.config.provider,
            model: &ctx.config.model,
            reasoning_effort: ctx.config.reasoning_effort.as_str(),
            vt_cfg: ctx.vt_cfg.as_ref(),
            provider_client: ctx.provider_client.as_ref(),
            tool_registry: ctx.tool_registry,
            active_thread_label: ctx.active_thread_label,
            session_stats: ctx.session_stats,
        }
    }
}

/// Refresh UI-facing runtime state on the interaction loop's cadence.
///
/// This boundary owns only status, IDE, and title synchronization; turn
/// execution and MCP lifecycle remain in the orchestration loop.
pub(super) async fn refresh_interaction_ui(
    mut ctx: StatusRefreshContext<'_>,
    state: &mut InteractionState<'_>,
    request: StatusRefreshRequest,
) {
    refresh_image_capability(&ctx);
    refresh_ide_context(&mut ctx, request);
    refresh_runtime_counters(&mut ctx, state).await;
    refresh_status_line(&ctx, state, request).await;
    refresh_balance(&ctx, state).await;
    sync_terminal_title(&ctx, state);
}

fn refresh_image_capability(ctx: &StatusRefreshContext<'_>) {
    let provider_supports_vision = ctx.provider_client.supports_vision(ctx.model);
    let model_supports_image_input =
        selected_model_supports_image_input(ctx.provider, ctx.model, provider_supports_vision);
    ctx.handle.set_image_input_enabled(model_supports_image_input);
}

fn refresh_ide_context(ctx: &mut StatusRefreshContext<'_>, request: StatusRefreshRequest) {
    let live_ide_context = refresh_live_ide_context_update(ctx.ide_context_bridge);
    if live_ide_context.changed || request.reason.configuration_reloaded() {
        apply_ide_context_snapshot(
            ctx.context_manager,
            ctx.header_context,
            ctx.handle,
            ctx.workspace,
            ctx.vt_cfg,
            live_ide_context.snapshot,
        );
    }
}

async fn refresh_runtime_counters(ctx: &mut StatusRefreshContext<'_>, state: &mut InteractionState<'_>) {
    let spooled_count = ctx.tool_registry.spooled_files_count().await;
    status_line::update_spooled_files_count(state.input_status_state, spooled_count);

    let local_agent_count = if let Some(controller) = ctx.tool_registry.subagent_controller() {
        let entries = controller.status_entries().await;
        crate::agent::runloop::ui::sync_active_subagent_badges(ctx.header_context, ctx.handle, &entries);
        let delegated_count = entries.iter().filter(|entry| !entry.status.is_terminal()).count();
        let background_count = controller
            .background_status_entries()
            .await
            .into_iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    diapason_core::subagents::BackgroundSubprocessStatus::Starting
                        | diapason_core::subagents::BackgroundSubprocessStatus::Running
                ) || (entry.desired_enabled
                    && matches!(entry.status, diapason_core::subagents::BackgroundSubprocessStatus::Error))
            })
            .count();
        delegated_count + background_count
    } else {
        crate::agent::runloop::ui::sync_active_subagent_badges(ctx.header_context, ctx.handle, &[]);
        0
    };
    status_line::update_thread_context(state.input_status_state, ctx.active_thread_label, local_agent_count);

    let effective_model = resolve_effective_request_model(ctx.model, ctx.active_primary_agent.active());
    let context_limit_tokens =
        diapason_core::compaction::effective_context_budget(ctx.vt_cfg, ctx.provider_client, &effective_model);
    let context_used_tokens = ctx.context_manager.current_token_usage();
    status_line::update_context_budget(state.input_status_state, context_used_tokens, context_limit_tokens);
}

async fn refresh_status_line(
    ctx: &StatusRefreshContext<'_>,
    state: &mut InteractionState<'_>,
    request: StatusRefreshRequest,
) {
    let status = &mut *state.input_status_state;
    if request.reason.configuration_reloaded() {
        status_line::invalidate_for_config_reload(status);
    }

    let status_config = ctx.vt_cfg.map(|cfg| &cfg.ui.status_line);
    let new_show_costs = status_line::status_line_shows_auto_components(status_config)
        && matches!(ctx.provider_client.name(), "deepseek" | "openai");
    if status.show_costs != new_show_costs {
        status.show_costs = new_show_costs;
        status.status_dirty = true;
    }
    let new_cost = ctx.session_stats.total_cost_usd();
    if status.cost_usd != new_cost {
        status.cost_usd = new_cost;
        status.status_dirty = true;
    }
    let usage = ctx.session_stats.total_usage();
    let total_cache = usage.cached_input_tokens + usage.cache_creation_tokens;
    let new_cache_pct = (total_cache > 0).then(|| (usage.cached_input_tokens as f64 / total_cache as f64) * 100.0);
    if status.cache_hit_pct != new_cache_pct {
        status.cache_hit_pct = new_cache_pct;
        status.status_dirty = true;
    }

    // Only rebuild the status line when something actually changed or the
    // displayed clock second ticked. This keeps idle ticks out of the status
    // command, Git query, and layout paths.
    let clock_ticked = status_line::clock_second_ticked(status);
    if !status.status_dirty && !clock_ticked {
        return;
    }

    match status_line::update_input_status_if_changed(
        ctx.handle,
        ctx.workspace,
        ctx.model,
        ctx.reasoning_effort,
        status_config,
        status,
    )
    .await
    {
        Ok(()) => status.status_dirty = false,
        Err(error) => tracing::warn!("Failed to refresh status line: {}", error),
    }
}

async fn refresh_balance(ctx: &StatusRefreshContext<'_>, state: &mut InteractionState<'_>) {
    let status = &mut *state.input_status_state;
    let status_config = ctx.vt_cfg.map(|cfg| &cfg.ui.status_line);
    if status.show_costs {
        status_line::refresh_balance_info(
            ctx.provider_client,
            ctx.handle,
            ctx.workspace,
            ctx.model,
            ctx.reasoning_effort,
            status_config,
            status,
        )
        .await;
    } else {
        if status.balance.take().is_some() {
            status.status_dirty = true;
        }
        status.last_balance_refresh = None;
    }
}

fn sync_terminal_title(ctx: &StatusRefreshContext<'_>, state: &mut InteractionState<'_>) {
    let title_items = ctx.vt_cfg.and_then(|cfg| cfg.ui.terminal_title.items.as_deref());
    status_line::sync_terminal_title(ctx.handle, state.input_status_state, title_items);
}
