//! Shared serialization for ACP tests that touch process-global history state.
//!
//! Session history persistence is configured through a process-global
//! `OnceLock`, so every test that reads or writes it must hold the same lock.
//! Handler tests and session-state tests run in one test binary; a second lock
//! would let them race.
use std::sync::LazyLock;
use tokio::sync::MutexGuard;
use diapason_config::codex::HistoryPersistence;

pub(crate) static SESSION_HISTORY_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Serializes tests that depend on the process-global history settings.
pub(crate) async fn lock_history_test_guard() -> MutexGuard<'static, ()> {
    SESSION_HISTORY_TEST_LOCK.lock().await
}

/// Sets the process-global history settings for the guard's lifetime.
///
/// Hold [`lock_history_test_guard`] for as long as this guard lives.
pub(crate) struct HistorySettingsGuard;

impl HistorySettingsGuard {
    pub(crate) fn set(persistence: HistoryPersistence, max_bytes: Option<usize>) -> Self {
        let mut config = diapason_core::config::DiapasonConfig::default();
        config.history.persistence = persistence;
        config.history.max_bytes = max_bytes;
        diapason_core::utils::session_archive::apply_session_history_config_from_diapason(&config);
        Self
    }
}

impl Drop for HistorySettingsGuard {
    fn drop(&mut self) {
        diapason_core::utils::session_archive::apply_session_history_config_from_diapason(
            &diapason_core::config::DiapasonConfig::default(),
        );
    }
}
