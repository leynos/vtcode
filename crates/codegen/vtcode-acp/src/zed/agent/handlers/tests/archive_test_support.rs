//! Isolates ACP tests that persist session archives from the user's session directory.
use std::ffi::OsString;
use std::path::Path;
use std::sync::LazyLock;
use tokio::sync::MutexGuard;

static SESSION_HISTORY_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

pub(super) const SESSION_DIR_ENV: &str = vtcode_core::utils::session_archive::SESSION_DIR_ENV;

pub(super) async fn lock_history_test_guard() -> MutexGuard<'static, ()> {
    SESSION_HISTORY_TEST_LOCK.lock().await
}

pub(super) struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
    environment_lock: vtcode_commons::env_lock::EnvGuard,
}

impl EnvGuard {
    pub(super) fn set(key: &'static str, value: &Path) -> Self {
        let environment_lock = vtcode_commons::env_lock::lock();
        let previous = std::env::var_os(key);
        environment_lock.set_var(key, value.as_os_str());
        Self { key, previous, environment_lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        self.environment_lock.restore_var(self.key, self.previous.as_ref());
    }
}
