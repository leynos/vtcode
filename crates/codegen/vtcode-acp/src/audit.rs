use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use vtcode_config::safety::ToolAuditConfig;
use vtcode_safety::audit_log::{ToolAuditEntry, ToolAuditInvocation, ToolAuditLogger, ToolAuditStatus};

/// ACP's opt-in audit boundary. The logger is created once at startup; every
/// write and flush is dispatched to Tokio's blocking pool so ACP handlers do
/// not perform synchronous filesystem I/O on the async executor.
#[derive(Clone, Debug)]
pub(crate) struct AcpAuditLogger {
    logger: Arc<ToolAuditLogger>,
}

impl AcpAuditLogger {
    pub(crate) async fn from_config(config: &ToolAuditConfig) -> Result<Option<Self>> {
        if !config.enabled() {
            return Ok(None);
        }

        let path = vtcode_commons::paths::expand_tilde(&config.path().to_string_lossy());
        let max_size_bytes = config.max_size_bytes();
        let max_files = config.max_files();
        let logger = tokio::task::spawn_blocking(move || ToolAuditLogger::jsonl_file(path, max_size_bytes, max_files))
            .await
            .context("ACP audit logger startup task failed")?
            .context("failed to open ACP audit log")?;

        Ok(Some(Self { logger: Arc::new(logger) }))
    }

    pub(crate) async fn record(&self, entry: ToolAuditEntry) -> Result<()> {
        let logger = Arc::clone(&self.logger);
        tokio::task::spawn_blocking(move || {
            logger.record(entry);
            logger.flush();
        })
        .await
        .context("ACP audit logger write task failed")?;
        Ok(())
    }
}

pub(crate) fn timestamp_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use std::fs;

    #[tokio::test]
    async fn disabled_audit_does_not_create_a_file() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("audit.jsonl");
        // The path is intentionally not public; parsing gives us a configured
        // path while retaining the private-field API.
        let config: ToolAuditConfig =
            toml::from_str(&format!("enabled = false\npath = {:?}", path)).expect("audit config");
        assert!(AcpAuditLogger::from_config(&config).await.expect("startup").is_none());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn enabled_audit_writes_hash_only_jsonl_entry() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("audit.jsonl");
        let config: ToolAuditConfig =
            toml::from_str(&format!("enabled = true\npath = {:?}\nmax_size_bytes = 4096\nmax_files = 2", path))
                .expect("audit config");
        let logger = AcpAuditLogger::from_config(&config)
            .await
            .expect("startup")
            .expect("enabled logger");
        logger
            .record(ToolAuditEntry::from_invocation(ToolAuditInvocation {
                timestamp_unix_ms: timestamp_unix_ms(),
                session_id: "session-1",
                turn_id: "turn-1",
                tool_call_id: "call-1",
                tool_name: "read_file",
                arguments: br#"{"path":"secret.txt"}"#,
                result: br#"{"status":"success","result":"secret"}"#,
                duration_ms: 3,
                status: ToolAuditStatus::Success,
                model_id: Some("test-model"),
                reason: None,
            }))
            .await
            .expect("record");

        let line = fs::read_to_string(path).expect("audit line");
        let value: serde_json::Value = serde_json::from_str(line.trim()).expect("jsonl");
        assert_eq!(value["status"], "success");
        assert_eq!(value["session_id"], "session-1");
        assert!(value.get("arguments_redacted").is_none());
        assert!(value.get("result_summary").is_none());
        assert!(value.get("secret").is_none());
    }
}
