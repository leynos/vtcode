//! Shared guarded setup for the Lody prompt-delegation wire tests.

use super::lody_prompt_delegation::install_agent_tool_provider;
use super::*;
use rstest::fixture;

/// Every guard the delegation tests must own for their whole test body.
///
/// The fields are declared in reverse acquisition order, so dropping the
/// bundle releases the provider factory, the provider lock, the session
/// directory override, the temporary directory and the history guard in that
/// order. Keep this declaration order when adding a guard.
pub(super) struct DelegationTestGuards {
    /// Counts the mocked model's provider calls.
    pub(super) provider_calls: Arc<AtomicUsize>,
    /// Records each provider request the mocked model receives.
    pub(super) provider_requests: Arc<Mutex<Vec<LLMRequest>>>,
    _factory_guard: PromptProviderFactoryGuard,
    _provider_lock: tokio::sync::MutexGuard<'static, ()>,
    _session_dir_guard: EnvGuard,
    _session_dir: TempDir,
    _history_test_guard: tokio::sync::MutexGuard<'static, ()>,
}

/// Acquires the shared delegation guards for the whole test body.
///
/// The guards are taken in one fixed order, and every delegation test uses
/// this fixture so all three serialize identically:
///
/// 1. the history test guard,
/// 2. the temporary ACP session directory and its `SESSION_DIR_ENV` override,
/// 3. the prompt-provider test lock,
/// 4. the mocked agent-tool provider installation.
///
/// This order must not be reversed. These are process-global guards shared
/// with the other ACP handler tests, so a test that acquires them in a
/// different order can hold what its peer is waiting for while that peer
/// holds what it needs, and both deadlock. rstest binds the returned bundle as
/// a test local, so every guard lives until the test ends.
#[fixture]
pub(super) async fn delegation_test_guards() -> DelegationTestGuards {
    let history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let provider_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let (provider_calls, provider_requests, factory_guard) = install_agent_tool_provider();
    DelegationTestGuards {
        provider_calls,
        provider_requests,
        _factory_guard: factory_guard,
        _provider_lock: provider_lock,
        _session_dir_guard: session_dir_guard,
        _session_dir: session_dir,
        _history_test_guard: history_test_guard,
    }
}
