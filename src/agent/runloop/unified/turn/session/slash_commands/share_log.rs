//! `/share-log` export handling: builds, redacts and writes the session log
//! JSON, Markdown and self-contained timeline HTML artefacts.

use std::fmt::Write;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use diapason_commons::sanitizer::redact_secrets;
use diapason_core::llm::provider as uni;
use diapason_core::utils::ansi::MessageStyle;
use diapason_core::utils::file_utils::write_file_with_context_sync;
use regex::Regex;
use serde_json::{Value, json};

use crate::agent::runloop::slash_commands::SessionLogExportFormat;

use super::{SlashCommandContext, SlashCommandControl};

#[path = "share_log/timeline.rs"]
mod timeline;

use timeline::{build_timeline_export, redact_timeline_export, render_session_timeline_html};

static EMAIL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b").expect("valid email regex"));
static USER_PATH_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?P<prefix>/(?:Users|home)/)[^/\s]+").expect("valid user path regex"));

fn build_session_log_messages(messages: &[uni::Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let mut entry = json!({
                "role": format!("{:?}", msg.role),
                "content": msg.content.as_text(),
            });
            if let Some(tool_calls) = &msg.tool_calls {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "function": tc.function.as_ref().map(|f| json!({
                                "name": f.name,
                                "arguments": f.arguments,
                            })),
                        })
                    })
                    .collect();
                entry["tool_calls"] = Value::Array(calls);
            }
            if let Some(tool_call_id) = &msg.tool_call_id {
                entry["tool_call_id"] = Value::String(tool_call_id.clone());
            }
            entry
        })
        .collect()
}

fn render_session_log_markdown(
    exported_at: &str,
    model: &str,
    workspace: &std::path::Path,
    messages: &[Value],
) -> String {
    let mut markdown = String::new();
    markdown.push_str("# Diapason Session Log\n\n");
    let _ = writeln!(markdown, "- Exported at: {exported_at}");
    let _ = writeln!(markdown, "- Model: `{model}`");
    let _ = writeln!(markdown, "- Workspace: `{}`", redact_sensitive_text(&workspace.display().to_string()));
    let _ = writeln!(markdown, "- Total messages: {}\n", messages.len());
    markdown.push_str("## Messages\n\n");

    for (index, message) in messages.iter().enumerate() {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("Unknown");
        let content = message.get("content").and_then(Value::as_str).unwrap_or("");

        let _ = writeln!(markdown, "### {}. {}\n", index + 1, role);
        if content.trim().is_empty() {
            markdown.push_str("_No textual content._\n\n");
        } else {
            markdown.push_str("```text\n");
            markdown.push_str(content);
            if !content.ends_with('\n') {
                markdown.push('\n');
            }
            markdown.push_str("```\n\n");
        }

        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array)
            && !tool_calls.is_empty()
        {
            markdown.push_str("Tool calls:\n");
            for call in tool_calls {
                let id = call.get("id").and_then(Value::as_str).unwrap_or("unknown");
                let function = call.get("function");
                let function_name = function
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str)
                    .map(canonical_tool_name)
                    .unwrap_or_else(|| "unknown".to_string());
                let _ = writeln!(markdown, "- `{id}`: `{function_name}`");

                if let Some(arguments) = function.and_then(|value| value.get("arguments")) {
                    let arguments_text =
                        serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
                    markdown.push_str("```json\n");
                    markdown.push_str(&arguments_text);
                    markdown.push_str("\n```\n");
                }
            }
            markdown.push('\n');
        }

        if let Some(tool_call_id) = message.get("tool_call_id").and_then(Value::as_str) {
            let _ = writeln!(markdown, "Tool call id: `{tool_call_id}`\n");
        }
    }

    markdown
}

fn redact_sensitive_text(input: &str) -> String {
    let mut redacted = redact_secrets(input.to_string());

    if let Some(home_dir) = std::env::var_os("HOME")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
    {
        redacted = redacted.replace(&home_dir, "~");
    }

    redacted = USER_PATH_REGEX.replace_all(&redacted, "${prefix}[REDACTED]").into_owned();
    EMAIL_REGEX.replace_all(&redacted, "[REDACTED_EMAIL]").into_owned()
}

fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_sensitive_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(key, value)| (key.clone(), redact_json_value(value))).collect())
        }
        _ => value.clone(),
    }
}

fn canonical_tool_name(name: &str) -> String {
    diapason_core::tools::tool_intent::canonical_command_session_tool_name(name)
        .unwrap_or(name)
        .to_string()
}

pub(crate) async fn handle_share_log(
    ctx: SlashCommandContext<'_>,
    format: SessionLogExportFormat,
) -> Result<SlashCommandControl> {
    use chrono::Local;

    let exported_at = Local::now().to_rfc3339();
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let log_messages = build_session_log_messages(ctx.conversation_history);
    let redacted_log_messages: Vec<Value> = log_messages.iter().map(redact_json_value).collect();
    let thread_events = ctx.thread_handle.replay_recent();
    let redacted_session_log_export = json!({
        "exported_at": exported_at,
        "provider": ctx.provider_client.name(),
        "model": &ctx.config.model,
        "workspace": redact_sensitive_text(&ctx.config.workspace.display().to_string()),
        "redaction_enabled": true,
        "total_messages": redacted_log_messages.len(),
        "messages": redacted_log_messages,
    });
    let json_output_path = ctx.config.workspace.join(format!("diapason-session-log-{timestamp}.json"));
    let markdown_output_path = ctx.config.workspace.join(format!("diapason-session-log-{timestamp}.md"));
    let html_output_path = ctx.config.workspace.join(format!("diapason-session-timeline-{timestamp}.html"));

    if matches!(format, SessionLogExportFormat::Both | SessionLogExportFormat::Json) {
        let json =
            serde_json::to_string_pretty(&redacted_session_log_export).context("Failed to serialize session log")?;
        write_file_with_context_sync(&json_output_path, &json, "session log")?;
    }

    if matches!(format, SessionLogExportFormat::Markdown) {
        let markdown = render_session_log_markdown(
            &exported_at,
            &ctx.config.model,
            &ctx.config.workspace,
            redacted_session_log_export
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        );
        write_file_with_context_sync(&markdown_output_path, &markdown, "session log")?;
    }

    if matches!(format, SessionLogExportFormat::Both | SessionLogExportFormat::Html) {
        let timeline_export = redact_timeline_export(&build_timeline_export(
            &exported_at,
            ctx.provider_client.name(),
            &ctx.config.model,
            &ctx.config.workspace,
            ctx.thread_id,
            &thread_events,
            ctx.conversation_history,
            Some(&ctx.session_stats.prompt_cache_diagnostics()),
        ));
        let html = render_session_timeline_html(&timeline_export, &redacted_session_log_export)?;
        write_file_with_context_sync(&html_output_path, &html, "session timeline")?;
    }

    match format {
        SessionLogExportFormat::Both => {
            ctx.renderer.line(
                MessageStyle::Info,
                &format!(
                    "Share exports ready:\nJSON: {}\nHTML: {}\nHTML is self-contained for offline sharing; JSON is useful for debugging.",
                    json_output_path.display(),
                    html_output_path.display()
                ),
            )?;
        }
        SessionLogExportFormat::Html => {
            ctx.renderer.line(
                MessageStyle::Info,
                &format!(
                    "Share HTML ready:\n{}\nThis HTML file is self-contained and can be shared offline.",
                    html_output_path.display()
                ),
            )?;
        }
        SessionLogExportFormat::Json => {
            ctx.renderer.line(
                MessageStyle::Info,
                &format!(
                    "Share JSON ready:\n{}\nYou can share this file for debugging purposes.",
                    json_output_path.display()
                ),
            )?;
        }
        SessionLogExportFormat::Markdown => {
            ctx.renderer.line(
                MessageStyle::Info,
                &format!("Session log exported to: {} ({})", markdown_output_path.display(), "Markdown"),
            )?;
            ctx.renderer
                .line(MessageStyle::Info, "You can share this file for debugging purposes.")?;
        }
    }

    Ok(SlashCommandControl::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diapason_core::config::loader::DiapasonConfig;
    use diapason_core::core::threads::{ThreadBootstrap, ThreadManager};
    use diapason_core::exec::events::{ThreadEvent, TurnCompletedEvent, Usage};
    use diapason_ui::tui::app::InlineHeaderContext;
    use hashbrown::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    use crate::agent::runloop::model_picker::ModelPickerState;
    use crate::agent::runloop::unified::palettes::ActivePalette;
    use crate::agent::runloop::unified::session_setup::IdeContextBridge;
    use crate::agent::runloop::unified::turn::turn_processing::test_support::TestTurnProcessingBacking;
    use crate::agent::runloop::unified::workspace_links::LinkedDirectory;
    use crate::agent::runloop::welcome::SessionBootstrap;
    use diapason_core::skills::types::Skill;

    /// Drive the real slash-command handler, not just the timeline helpers, so
    /// the wiring from `SessionStats` through the export writer is covered.
    ///
    /// Executing the page's JavaScript is out of scope for a unit test (this
    /// workspace has no JS runtime or DOM harness), so the assertions read the
    /// written artefact and target the renderer's binding plus the data blob
    /// the page renders from.
    #[tokio::test]
    async fn handle_share_log_html_export_renders_catalogue_change_count() {
        let mut backing = TestTurnProcessingBacking::new(4).await;
        let workspace = backing.workspace_path().to_path_buf();

        let thread_handle = Box::leak(Box::new(
            ThreadManager::new().start_thread_with_identifier("test-thread", ThreadBootstrap::new(None)),
        ));
        thread_handle.record_event(
            None,
            Some("turn-1".to_string()),
            ThreadEvent::TurnCompleted(TurnCompletedEvent {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 4,
                    cached_input_tokens: 2,
                    cache_creation_tokens: 0,
                },
            }),
        );

        let turn = backing.turn_loop_context();

        // Five fingerprint observations across four distinct catalogue hashes
        // produce `observations: 5` and `tool_catalogue_changes: 3`.
        for catalogue_hash in [10_u64, 11, 12, 13] {
            let _ = turn
                .session_stats
                .record_prompt_cache_fingerprint("noop-model", 1, Some(catalogue_hash));
        }
        let _ = turn.session_stats.record_prompt_cache_fingerprint("noop-model", 1, Some(13));
        let diagnostics = turn.session_stats.prompt_cache_diagnostics();
        assert_eq!(diagnostics.observations, 5);
        assert_eq!(diagnostics.tool_catalogue_changes, 3);

        let header_context = Box::leak(Box::new(InlineHeaderContext::default()));
        let ide_context_bridge = Box::leak(Box::new(None::<IdeContextBridge>));
        let model_picker_state = Box::leak(Box::new(None::<ModelPickerState>));
        let palette_state = Box::leak(Box::new(None::<ActivePalette>));
        let conversation_history = Box::leak(Box::new(Vec::<uni::Message>::new()));
        let loaded_skills = Box::leak(Box::new(Arc::new(RwLock::new(HashMap::<String, Skill>::new()))));
        let linked_directories = Box::leak(Box::new(Vec::<LinkedDirectory>::new()));
        let session_bootstrap = Box::leak(Box::new(SessionBootstrap::default()));
        let vt_cfg = Box::leak(Box::new(None::<DiapasonConfig>));
        let async_mcp_manager = None;
        let checkpoint_manager = None;
        let lifecycle_hooks = None;
        let harness_emitter = None;
        let (webmcp_prompt_sender, _webmcp_prompt_receiver) = crate::agent::runloop::unified::webmcp::prompt_channel();
        let webmcp_bridge = Box::leak(Box::new(None));
        let active_primary_agent =
            Box::leak(Box::new(diapason_core::primary_agent::ActivePrimaryAgentState::default()));
        let plan_session = Box::leak(Box::new(
            crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default(),
        ));

        let ctx = SlashCommandContext {
            thread_id: "test-thread",
            active_thread_label: "main",
            thread_handle,
            renderer: turn.renderer,
            handle: turn.handle,
            session: turn.session,
            header_context,
            ide_context_bridge,
            config: turn.config,
            vt_cfg,
            provider_client: turn.provider_client,
            session_bootstrap,
            model_picker_state,
            palette_state,
            tool_registry: turn.tool_registry,
            conversation_history,
            decision_ledger: turn.decision_ledger,
            context_manager: turn.context_manager,
            active_primary_agent,
            session_stats: turn.session_stats,
            plan_session,
            input_status_state: turn.input_status_state,
            tools: turn.tools,
            tool_catalogue: turn.tool_catalogue,
            async_mcp_manager,
            mcp_panel_state: turn.mcp_panel_state,
            linked_directories,
            ctrl_c_state: turn.ctrl_c_state,
            ctrl_c_notify: turn.ctrl_c_notify,
            full_auto: turn.full_auto,
            webmcp_prompt_sender: &webmcp_prompt_sender,
            webmcp_bridge,
            loaded_skills,
            checkpoint_manager,
            lifecycle_hooks,
            harness_emitter,
        };

        let control = handle_share_log(ctx, SessionLogExportFormat::Html)
            .await
            .expect("share-log html export should succeed");
        assert!(matches!(control, SlashCommandControl::Continue));

        let html_path = std::fs::read_dir(&workspace)
            .expect("read export workspace")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("diapason-session-timeline-") && name.ends_with(".html"))
            })
            .expect("share-log handler should write a timeline HTML artefact");
        let html = std::fs::read_to_string(&html_path).expect("read exported timeline html");

        assert!(
            html.contains("tool_catalogue: ${formatNumber(overview.prompt_cache_tool_catalog_changes)}"),
            "exported HTML does not bind the catalogue label to its count"
        );
        assert!(
            html.contains("\"prompt_cache_tool_catalog_changes\":3"),
            "exported HTML data blob lost the wire field name or count"
        );
    }
}
