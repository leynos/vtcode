//! Shared mock providers and session setup for ACP prompt-handler tests.
use super::*;
use agent_client_protocol::schema::v1::NewSessionRequest;

pub(in crate::zed::agent::handlers) static PROMPT_PROVIDER_TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

pub(in crate::zed::agent::handlers) struct PromptProviderFactoryGuard;

impl PromptProviderFactoryGuard {
    pub(in crate::zed::agent::handlers) fn install(provider_name: &str, factory: Arc<PromptProviderFactory>) -> Self {
        *PROMPT_PROVIDER_OVERRIDE.lock().expect("prompt provider factory lock") =
            Some(PromptProviderOverride { provider_name: provider_name.to_string(), factory });
        Self
    }
}

impl Drop for PromptProviderFactoryGuard {
    fn drop(&mut self) {
        *PROMPT_PROVIDER_OVERRIDE.lock().expect("prompt provider factory lock") = None;
    }
}

pub(super) struct FailThenSucceedProvider {
    pub(super) calls: Arc<AtomicUsize>,
}

pub(super) struct StreamToolThenAnswerProvider {
    pub(super) emit_usage: bool,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) requests: Arc<Mutex<Vec<LLMRequest>>>,
    pub(super) tool_name: &'static str,
    pub(super) tool_arguments: String,
    pub(super) additional_tool_calls: Vec<vtcode_core::llm::provider::ToolCall>,
}

pub(super) struct CompactThenAnswerProvider {
    pub(super) requests: Arc<Mutex<Vec<LLMRequest>>>,
    pub(super) context_size: usize,
    pub(super) summary: String,
}

pub(super) struct PendingCompactionProvider {
    started: Arc<tokio::sync::Notify>,
}

impl PendingCompactionProvider {
    pub(super) fn new(started: Arc<tokio::sync::Notify>) -> Self {
        Self { started }
    }
}

pub(super) struct PartialThenFailProvider;

#[async_trait]
impl LLMProvider for StreamToolThenAnswerProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.requests.lock().expect("stream requests").push(request);
        let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if response_index == 0 {
            let mut tool_calls = vec![vtcode_core::llm::provider::ToolCall::function(
                "call-tool".to_string(),
                self.tool_name.to_string(),
                self.tool_arguments.clone(),
            )];
            tool_calls.extend(self.additional_tool_calls.clone());
            let response = LLMResponse {
                content: Some("Checking files.".to_string()),
                reasoning: Some("I need the workspace listing.".to_string()),
                tool_calls: Some(tool_calls),
                finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
                model: "wire-model".to_string(),
                usage: self.emit_usage.then(|| vtcode_core::llm::Usage {
                    prompt_tokens: u32::try_from(100 + response_index).expect("fixture usage fits u32"),
                    completion_tokens: if response_index == 0 { 11 } else { 12 },
                    ..Default::default()
                }),
                ..LLMResponse::default()
            };
            vec![
                Ok(LLMStreamEvent::Reasoning { delta: "I need the workspace listing.".to_string() }),
                Ok(LLMStreamEvent::Token { delta: "Checking files.".to_string() }),
                Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
            ]
        } else {
            let response = LLMResponse {
                content: Some("Tool complete.".to_string()),
                finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                model: "wire-model".to_string(),
                usage: self.emit_usage.then(|| vtcode_core::llm::Usage {
                    prompt_tokens: u32::try_from(100 + response_index).expect("fixture usage fits u32"),
                    completion_tokens: if response_index == 0 { 11 } else { 12 },
                    ..Default::default()
                }),
                ..LLMResponse::default()
            };
            vec![
                Ok(LLMStreamEvent::Token { delta: "Tool complete.".to_string() }),
                Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Err(LLMError::InvalidRequest {
            message: "tool-enabled ACP turn used buffered generation".to_string(),
            metadata: None,
        })
    }
}

#[async_trait]
impl LLMProvider for CompactThenAnswerProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        self.context_size
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.requests.lock().expect("compaction provider requests").push(request);
        let response = LLMResponse {
            content: Some("Answer after compaction.".to_string()),
            finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
            model: "wire-model".to_string(),
            ..LLMResponse::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(LLMStreamEvent::Token { delta: "Answer after compaction.".to_string() }),
            Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
        ])))
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.requests.lock().expect("compaction provider requests").push(request);
        Ok(LLMResponse::new("wire-model", self.summary.clone()))
    }
}

#[async_trait]
impl LLMProvider for PendingCompactionProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        8_192
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.started.notify_one();
        std::future::pending().await
    }
}

#[async_trait]
impl LLMProvider for PartialThenFailProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(LLMStreamEvent::Token { delta: "partial answer".to_string() }),
            Err(LLMError::Network {
                message: "fixture stream disconnected".to_string(),
                metadata: None,
            }),
        ])))
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("partial-stream test must not use buffered generation")
    }
}

#[async_trait]
impl LLMProvider for FailThenSucceedProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_tools(&self, _model: &str) -> bool {
        false
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(LLMError::InvalidRequest {
                message: "\u{1b}[31mfirst turn failed\u{1b}[0m".to_string(),
                metadata: None,
            })
        } else {
            Ok(LLMResponse::new("wire-model", "second turn succeeded"))
        }
    }
}

pub(super) async fn build_wire_test_agent(workspace: &std::path::Path) -> ZedAgent {
    build_wire_test_agent_with_options(workspace, None, None).await
}

pub(super) async fn build_wire_test_agent_with_options(
    workspace: &std::path::Path,
    vt_config: Option<&VTCodeConfig>,
    audit_logger: Option<Arc<crate::audit::AcpAuditLogger>>,
) -> ZedAgent {
    build_wire_test_agent_with_providers_and_options(workspace, &[], vt_config, audit_logger).await
}

pub(in crate::zed::agent::handlers) async fn build_wire_test_agent_with_providers(
    workspace: &std::path::Path,
    providers: &[CustomProviderConfig],
) -> ZedAgent {
    build_wire_test_agent_with_providers_and_options(workspace, providers, None, None).await
}

pub(super) async fn build_wire_test_agent_with_providers_and_options(
    workspace: &std::path::Path,
    providers: &[CustomProviderConfig],
    vt_config: Option<&VTCodeConfig>,
    audit_logger: Option<Arc<crate::audit::AcpAuditLogger>>,
) -> ZedAgent {
    let audit_logger = match (audit_logger, vt_config) {
        (Some(audit_logger), _) => Some(audit_logger),
        (None, Some(vt_config)) => crate::audit::initialize_acp_audit_logger(vt_config)
            .await
            .expect("initialize ACP audit logger from runtime config"),
        (None, None) => None,
    };
    let core_config = CoreAgentConfig {
        model: "wire-model".to_string(),
        api_key: "test-key".to_string(),
        provider: "wire-test".to_string(),
        api_key_env: "WIRE_TEST_API_KEY".to_string(),
        workspace: workspace.to_path_buf(),
        verbose: false,
        quiet: false,
        theme: "test".to_string(),
        reasoning_effort: ReasoningEffortLevel::Low,
        ui_surface: UiSurfacePreference::default(),
        prompt_cache: PromptCachingConfig::default(),
        model_source: ModelSelectionSource::WorkspaceConfig,
        custom_api_keys: BTreeMap::new(),
        checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
        checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
        max_conversation_turns: 1000,
        model_behaviour: None,
        openai_chatgpt_auth: None,
    };
    let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
    discovery_input.include_user_agents = false;
    let discovered = vtcode_config::discover_subagents(&discovery_input).expect("discover primary agents");
    let primary_agents =
        crate::zed::helpers::PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, "duck");

    let mut zed_config = AgentClientProtocolZedConfig::default();
    zed_config.allowed_workspace_roots.push(std::env::temp_dir());

    Box::pin(ZedAgent::new(ZedAgentSetup {
        config: core_config,
        credential_storage_mode: AuthCredentialsStoreMode::default(),
        zed_config,
        tools_config: ToolsConfig::default(),
        commands_config: CommandsConfig::default(),
        custom_providers: providers,
        provider_timeouts: vtcode_config::TimeoutsConfig::default(),
        system_prompt: String::new(),
        title: Some("Wire test".to_string()),
        primary_agents,
        skip_confirmations: true,
        vt_config,
        audit_logger,
    }))
    .await
    .expect("build test ACP agent")
}

pub(super) async fn large_compaction_session(agent: &ZedAgent, workspace: &std::path::Path) -> SessionHandle {
    let response = agent
        .new_session(NewSessionRequest::new(workspace))
        .await
        .expect("create ACP compaction session");
    let session = agent
        .session_handle(&response.session_id)
        .expect("ACP session lookup")
        .expect("new ACP session handle");
    for index in 0..12 {
        agent
            .push_message(
                &session,
                Message::user(format!("large user turn {index} {}", "older context ".repeat(12_000))),
            )
            .expect("append large user history");
        agent
            .push_message(&session, Message::assistant(format!("large assistant turn {index}")))
            .expect("append large assistant history");
    }
    session
}

pub(super) fn compaction_config(enabled: bool) -> VTCodeConfig {
    let mut config = VTCodeConfig::default();
    config.agent.harness.auto_compaction_enabled = enabled;
    config.agent.harness.auto_compaction_threshold_tokens = Some(1);
    config
}
