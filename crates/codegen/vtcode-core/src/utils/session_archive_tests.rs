//! Tests session archive serialization, restoration, normalization, and history retention behavior.
use super::*;
use crate::config::constants::tools as tool_names;
use crate::core::agent::snapshots::SnapshotTurnDiagnostics;
use crate::llm::provider::{
    AssistantPhase, ContentPart, Message, MessageClearAt, MessageContent, MessageRole, ToolCall,
};
use crate::utils::error_log_collector::ErrorLogEntry;
use anyhow::anyhow;
use chrono::{TimeZone, Timelike, Utc};
use std::fs;
use std::mem::size_of;
use std::path::Path;
use std::process;
use std::sync::LazyLock;
use std::time::Duration;

static SESSION_HISTORY_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

struct EnvGuard {
    key: &'static str,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        set_test_env_override_path(key, value);
        Self { key }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        clear_test_env_override(self.key);
    }
}

struct HistorySettingsGuard {
    previous: SessionHistorySettings,
}

impl HistorySettingsGuard {
    fn set(persistence: HistoryPersistence, max_bytes: Option<usize>) -> Self {
        let previous = session_history_settings();
        let mut config = VTCodeConfig::default();
        config.history.persistence = persistence;
        config.history.max_bytes = max_bytes;
        apply_session_history_config_from_vtcode(&config);
        Self { previous }
    }
}

impl Drop for HistorySettingsGuard {
    fn drop(&mut self) {
        let mut config = VTCodeConfig::default();
        config.history.persistence = self.previous.persistence;
        config.history.max_bytes = self.previous.max_bytes;
        apply_session_history_config_from_vtcode(&config);
    }
}

async fn lock_history_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    SESSION_HISTORY_TEST_LOCK.lock().await
}

#[path = "session_archive_tests/archive.rs"]
mod archive;
#[path = "session_archive_tests/listing.rs"]
mod listing;
#[path = "session_archive_tests/messages.rs"]
mod messages;
#[path = "session_archive_tests/progress.rs"]
mod progress;
#[path = "session_archive_tests/retention.rs"]
mod retention;
#[path = "session_archive_tests/snapshot_compaction.rs"]
mod snapshot_compaction;
#[path = "session_archive_tests/transcript.rs"]
mod transcript;

#[path = "session_archive_tests/metadata.rs"]
mod metadata;
