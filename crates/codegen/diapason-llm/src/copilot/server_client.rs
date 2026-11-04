use std::path::Path;
use std::str;

use anyhow::{Context, Result, anyhow};
use diapason_commons::sanitizer::{PROVIDER_DIAGNOSTIC_MAX_BYTES, sanitize_provider_diagnostic};
use diapason_config::auth::CopilotAuthConfig;
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::time::timeout;

use super::command::{resolve_copilot_command, spawn_copilot_server_process};
use super::transport::{read_bounded_line, trim_line_ending};
use super::types::CopilotDiscoveredModel;

const MAX_COPILOT_HEADER_BYTES: usize = 8 * 1024;

pub async fn list_available_models(
    config: &CopilotAuthConfig,
    workspace_root: &Path,
) -> Result<Vec<CopilotDiscoveredModel>> {
    let resolved = resolve_copilot_command(config)?;
    let mut child = spawn_copilot_server_process(&resolved, workspace_root)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("copilot cli server stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("copilot cli server stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("copilot cli server stderr unavailable"))?;

    spawn_server_stderr(stderr);

    let result = timeout(resolved.startup_timeout, async move {
        let mut writer = stdin;
        let mut reader = BufReader::new(stdout);

        send_request(&mut writer, 1, "ping", Some(json!({ "message": "diapason model discovery" })))
            .await
            .context("copilot cli ping")?;
        let ping = read_response(&mut reader, 1).await.context("copilot cli ping")?;
        let protocol_version = ping.get("protocolVersion").and_then(Value::as_i64).unwrap_or(0);
        if protocol_version <= 0 {
            return Err(anyhow!("copilot cli server did not report a protocol version"));
        }

        send_request(&mut writer, 2, "models.list", None)
            .await
            .context("copilot cli models.list")?;
        let payload = read_response(&mut reader, 2).await.context("copilot cli models.list")?;
        let models = payload
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("copilot cli models.list response missing models"))?;

        let mut discovered = Vec::new();
        for model in models {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let name = model
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let policy_enabled = model
                .get("policy")
                .and_then(Value::as_object)
                .and_then(|policy| policy.get("state"))
                .and_then(Value::as_str)
                .map(|state| state.eq_ignore_ascii_case("enabled"))
                .unwrap_or(true);
            if !policy_enabled {
                continue;
            }
            let Some(id) = id else {
                continue;
            };
            discovered.push(CopilotDiscoveredModel {
                id: id.to_string(),
                name: name.unwrap_or(id).to_string(),
            });
        }

        discovered.sort_by(|left, right| left.id.cmp(&right.id));
        discovered.dedup_by(|left, right| left.id.eq_ignore_ascii_case(&right.id));
        Ok::<Vec<CopilotDiscoveredModel>, anyhow::Error>(discovered)
    })
    .await;

    terminate_child(&mut child).await;
    let result = result.context("copilot cli model discovery timeout")??;
    Ok(result)
}

async fn terminate_child(child: &mut Child) {
    if let Err(error) = child.start_kill() {
        tracing::debug!(target: "copilot.server", error = %error, "copilot cli child already stopped");
    }
    if let Err(error) = child.wait().await {
        tracing::debug!(target: "copilot.server", error = %error, "failed to reap copilot cli child");
    }
}

fn spawn_server_stderr(stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::with_capacity(PROVIDER_DIAGNOSTIC_MAX_BYTES);
        loop {
            match read_bounded_line(&mut reader, &mut line, PROVIDER_DIAGNOSTIC_MAX_BYTES).await {
                Ok(Some(_truncated)) => {
                    let trimmed = trim_line_ending(&line);
                    if !trimmed.iter().all(u8::is_ascii_whitespace) {
                        let safe_line = sanitize_provider_diagnostic(trimmed);
                        tracing::debug!(target: "copilot.server.stderr", "{}", safe_line);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(target: "copilot.server.stderr", error = %error, "stderr reader failed");
                    break;
                }
            }
        }
    });
}

async fn send_request<W>(writer: &mut W, id: i64, method: &str, params: Option<Value>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let message = if let Some(params) = params {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        })
    };
    let payload = serde_json::to_vec(&message).context("copilot cli json serialization failed")?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes())
        .await
        .context("copilot cli write header failed")?;
    writer.write_all(&payload).await.context("copilot cli write payload failed")?;
    writer.flush().await.context("copilot cli flush failed")?;
    Ok(())
}

async fn read_response(reader: &mut BufReader<ChildStdout>, expected_id: i64) -> Result<Value> {
    loop {
        let message = read_message(reader).await?;
        let Some(object) = message.as_object() else {
            continue;
        };

        if object.get("method").is_some() {
            continue;
        }

        if let Some(error) = object.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or_default();
            let detail = error.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(anyhow!("copilot cli rpc error {code}: {detail}"));
        }

        if object.get("id").and_then(Value::as_i64) != Some(expected_id) {
            continue;
        }

        return object
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("copilot cli rpc response missing result"));
    }
}

/// Upper bound on a single Copilot CLI JSON-RPC payload.
///
/// `Content-Length` arrives from the spawned Copilot CLI child process and is
/// passed straight to `read_exact_uninit`, which allocates that many bytes up
/// front. A malformed or hostile stream advertising an enormous length would
/// otherwise drive the process into an out-of-memory abort. Copilot responses
/// (model lists, completions) are well under this cap; reject anything larger.
const MAX_COPILOT_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

async fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_length = None;
    let mut line = Vec::with_capacity(MAX_COPILOT_HEADER_BYTES);
    loop {
        let Some(truncated) = read_bounded_line(reader, &mut line, MAX_COPILOT_HEADER_BYTES)
            .await
            .context("copilot cli header read failed")?
        else {
            return Err(anyhow!("copilot cli server closed the stdio stream"));
        };
        if truncated {
            return Err(anyhow!("copilot cli header exceeds {MAX_COPILOT_HEADER_BYTES} byte limit"));
        }

        let trimmed_bytes = trim_line_ending(&line);
        let trimmed = str::from_utf8(trimmed_bytes).context("copilot cli header is not valid UTF-8")?;
        if trimmed.is_empty() {
            break;
        }

        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .context("invalid copilot cli content length header")?;
            if parsed > MAX_COPILOT_PAYLOAD_BYTES {
                return Err(anyhow!(
                    "copilot cli content length {parsed} exceeds {MAX_COPILOT_PAYLOAD_BYTES} byte limit"
                ));
            }
            content_length = Some(parsed);
        }
    }

    let content_length = content_length.ok_or_else(|| anyhow!("copilot cli response missing Content-Length"))?;
    let payload = diapason_commons::async_utils::read_exact_uninit(reader, content_length)
        .await
        .context("copilot cli payload read failed")?;
    serde_json::from_slice(&payload).context("copilot cli json decode failed")
}
