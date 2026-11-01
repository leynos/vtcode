//! Isolates ACP tests that persist session archives from the user's session directory.
use std::ffi::OsString;
use std::path::Path;

pub(super) use crate::zed::agent::test_support::lock_history_test_guard;

pub(super) const SESSION_DIR_ENV: &str = vtcode_core::utils::session_archive::SESSION_DIR_ENV;

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
