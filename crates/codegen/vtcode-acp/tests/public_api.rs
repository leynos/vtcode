//! Compile-time external-crate coverage for the ACP-facing public APIs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use vtcode_core::config::VTCodeConfig;
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{AgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};
use vtcode_core::core::interfaces::AcpLaunchParams;
use vtcode_llm::providers::{CustomProviderAdmissionMetrics, custom_provider_admission_metrics};
use vtcode_safety::audit_log::{ToolAuditInvocation, ToolAuditStatus};

#[test]
fn public_types_are_usable_from_an_external_crate() {
    let agent_config = AgentConfig {
        model: "test-model".to_owned(),
        api_key: "test-key".to_owned(),
        provider: "test-provider".to_owned(),
        openai_chatgpt_auth: None,
        api_key_env: "TEST_API_KEY".to_owned(),
        workspace: PathBuf::from("."),
        verbose: false,
        quiet: false,
        theme: "default".to_owned(),
        reasoning_effort: ReasoningEffortLevel::default(),
        ui_surface: UiSurfacePreference::default(),
        prompt_cache: PromptCachingConfig::default(),
        model_source: ModelSelectionSource::default(),
        custom_api_keys: BTreeMap::new(),
        checkpointing_enabled: false,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: 0,
        checkpointing_max_age_days: None,
        max_conversation_turns: 1,
        model_behaviour: None,
    };
    let runtime_config = VTCodeConfig::default();
    let launch_params = AcpLaunchParams::new(&agent_config, &runtime_config).with_skip_confirmations(true);
    assert!(launch_params.skip_confirmations);

    let invocation = ToolAuditInvocation {
        timestamp_unix_ms: 1,
        session_id: "session",
        turn_id: "turn",
        tool_call_id: "call",
        tool_name: "read_file",
        arguments: br#"{"path":"src/lib.rs"}"#,
        result: b"contents",
        duration_ms: 2,
        status: ToolAuditStatus::Success,
        model_id: Some("model"),
        reason: None,
    };
    assert_eq!(invocation.tool_call_id, "call");

    let metrics: CustomProviderAdmissionMetrics = custom_provider_admission_metrics();
    assert_eq!(metrics.queue_depth, 0);
}
