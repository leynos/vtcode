//! Shared support for API-key resolution and credential-source unit tests.

use super::*;
use std::sync::Mutex;
use tempfile::tempdir;

// Serialize all env-override tests so that one test's Drop restore cannot
// overwrite another test's set.
static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

struct EnvOverrideGuard {
    key: &'static str,
    previous: Option<Option<String>>,
}

impl EnvOverrideGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let previous = crate::env_helpers::test_env_overrides::get(key);
        crate::env_helpers::test_env_overrides::set(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvOverrideGuard {
    fn drop(&mut self) {
        crate::env_helpers::test_env_overrides::restore(self.key, self.previous.clone());
    }
}

fn with_override<F>(key: &'static str, value: Option<&str>, f: F)
where
    F: FnOnce(),
{
    let _lock = ENV_TEST_LOCK.lock().expect("env test lock poisoned");
    let _guard = EnvOverrideGuard::set(key, value);
    f();
}

fn with_overrides<F>(overrides: &[(&'static str, Option<&str>)], f: F)
where
    F: FnOnce(),
{
    let _lock = ENV_TEST_LOCK.lock().expect("env test lock poisoned");
    let _guards: Vec<_> = overrides
        .iter()
        .map(|(key, value)| EnvOverrideGuard::set(key, *value))
        .collect();
    f();
}

fn default_sources() -> ApiKeySources {
    ApiKeySources::default()
}

#[path = "api_key_lookup_tests.rs"]
mod api_key_lookup_tests;

#[path = "credential_resolution_tests.rs"]
mod credential_resolution_tests;

#[path = "provider_discovery_tests.rs"]
mod provider_discovery_tests;
