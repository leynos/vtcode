use super::{
    OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes, WorkspaceRuntimeCache, WorkspaceRuntimeLookup,
    discover_session_primary_agents, preserve_optional_workspace_setup_outcomes,
};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use vtcode_config::SubagentDiscoveryInput;
use vtcode_core::metrics::MetricsCollector;

fn workspace_runtime_cache<T>() -> WorkspaceRuntimeCache<T> {
    WorkspaceRuntimeCache::new(Arc::new(MetricsCollector::new()))
}

#[test]
fn workspace_agent_discovery_failure_keeps_its_stage_context() {
    let mut input = SubagentDiscoveryInput::new(PathBuf::from("/tmp/acp-session-workspace"));
    input.include_user_agents = false;
    input.cli_agents = Some(Value::Null);

    let error = match discover_session_primary_agents(&input, "duck") {
        Ok(_) => panic!("invalid CLI agents should fail discovery"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("discovering ACP workspace agents"));
}

#[test]
fn optional_workspace_setup_outcomes_preserve_component_degradation() {
    let outcomes = preserve_optional_workspace_setup_outcomes(
        OptionalSetupOutcome::AttachedWithDegradation(vec![
            "failed to restore background subagents: archive unavailable".to_string(),
        ]),
        OptionalSetupOutcome::DisabledByConfiguration,
    );

    assert_eq!(
        outcomes,
        OptionalWorkspaceSetupOutcomes {
            subagent_controller: OptionalSetupOutcome::AttachedWithDegradation(vec![
                "failed to restore background subagents: archive unavailable".to_string(),
            ]),
            mcp_client: OptionalSetupOutcome::DisabledByConfiguration,
        }
    );
}

#[test]
fn workspace_runtime_cache_releases_expired_runtimes_and_prunes_keys() {
    let workspace_root = PathBuf::from("/tmp/acp-cache-workspace");
    let mut cache = workspace_runtime_cache();
    let runtime = Arc::new(String::from("workspace runtime"));
    let weak_runtime = Arc::downgrade(&runtime);

    cache.remember_runtime(workspace_root.clone(), &runtime);
    assert_eq!(cache.metrics.get_acp_workspace_runtime_cache_entries(), 1);
    let cached_runtime = match cache.lookup(&workspace_root) {
        WorkspaceRuntimeLookup::Ready(cached_runtime) => cached_runtime,
        WorkspaceRuntimeLookup::Initialize(_) => panic!("live runtime should be reused"),
    };
    assert!(Arc::ptr_eq(&runtime, &cached_runtime));
    drop(cached_runtime);
    drop(runtime);

    assert!(weak_runtime.upgrade().is_none());
    let initializing = match cache.lookup(&workspace_root) {
        WorkspaceRuntimeLookup::Ready(_) => panic!("expired runtime should not be reused"),
        WorkspaceRuntimeLookup::Initialize(runtime_cell) => runtime_cell,
    };
    assert!(cache.runtimes.is_empty(), "expired workspace keys are pruned on lookup");
    assert_eq!(cache.metrics.get_acp_workspace_runtime_cache_entries(), 1);
    drop(initializing);

    let _next_workspace = cache.lookup(&PathBuf::from("/tmp/acp-next-workspace"));
    assert_eq!(cache.in_flight.len(), 1, "expired initialization keys are pruned on lookup");
    assert_eq!(cache.metrics.get_acp_workspace_runtime_cache_entries(), 1);
}

#[test]
fn workspace_runtime_cache_shares_initialization_per_key_only() {
    let first_workspace = PathBuf::from("/tmp/acp-first-workspace");
    let second_workspace = PathBuf::from("/tmp/acp-second-workspace");
    let mut cache = workspace_runtime_cache::<String>();

    let first_cell = match cache.lookup(&first_workspace) {
        WorkspaceRuntimeLookup::Initialize(runtime_cell) => runtime_cell,
        WorkspaceRuntimeLookup::Ready(_) => panic!("first workspace should initialize"),
    };
    let concurrent_cell = match cache.lookup(&first_workspace) {
        WorkspaceRuntimeLookup::Initialize(runtime_cell) => runtime_cell,
        WorkspaceRuntimeLookup::Ready(_) => panic!("workspace initialization is still in flight"),
    };
    let other_cell = match cache.lookup(&second_workspace) {
        WorkspaceRuntimeLookup::Initialize(runtime_cell) => runtime_cell,
        WorkspaceRuntimeLookup::Ready(_) => panic!("second workspace should initialize independently"),
    };

    assert!(Arc::ptr_eq(&first_cell, &concurrent_cell));
    assert!(!Arc::ptr_eq(&first_cell, &other_cell));
}
