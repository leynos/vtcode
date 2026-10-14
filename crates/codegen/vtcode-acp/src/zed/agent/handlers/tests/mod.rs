//! Shared fixtures and protocol harness for ACP prompt, provider, session, and tool tests.
use super::super::ZedAgentSetup;
use super::prompt::*;
use super::provider::*;
use super::*;

mod prompt;
mod provider;
mod session;
mod tools;

use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use async_trait::async_trait;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};
use vtcode_config::SubagentDiscoveryInput;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::core::threads::{ThreadBootstrap, ThreadManager};

static PROMPT_PROVIDER_TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

struct PromptProviderFactoryGuard;

impl PromptProviderFactoryGuard {
    fn install(provider_name: &str, factory: Arc<PromptProviderFactory>) -> Self {
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

#[derive(Clone, Default)]
struct TraceCapture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for TraceCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let current = context.lookup_current().map(|span| span.name().to_owned());
        let mut fields = CapturedFields::default();
        event.record(&mut fields);
        let mut line = format!("span={current:?} event={}", event.metadata().name());
        line.push_str(&fields.0);
        self.0.lock().expect("trace capture lock").push(line);
    }

    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, _id: &tracing::Id, _context: Context<'_, S>) {
        let mut fields = CapturedFields::default();
        attrs.record(&mut fields);
        let mut line = format!("span={}", attrs.metadata().name());
        line.push_str(&fields.0);
        self.0.lock().expect("trace capture lock").push(line);
    }

    fn on_record(&self, _id: &tracing::Id, values: &tracing::span::Record<'_>, _context: Context<'_, S>) {
        let mut fields = CapturedFields::default();
        values.record(&mut fields);
        self.0.lock().expect("trace capture lock").push(fields.0);
    }
}

#[derive(Default)]
struct CapturedFields(String);

impl Visit for CapturedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }
}

struct FailThenSucceedProvider {
    calls: Arc<AtomicUsize>,
}

struct StreamToolThenAnswerProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    tool_name: &'static str,
    tool_arguments: String,
    additional_tool_calls: Vec<vtcode_core::llm::provider::ToolCall>,
}

struct CompactThenAnswerProvider {
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    context_size: usize,
    summary: String,
}

struct PendingCompactionProvider {
    started: Arc<tokio::sync::Notify>,
}

impl PendingCompactionProvider {
    fn new(started: Arc<tokio::sync::Notify>) -> Self {
        Self { started }
    }
}

struct PartialThenFailProvider;

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

async fn build_wire_test_agent(workspace: &std::path::Path) -> ZedAgent {
    build_wire_test_agent_with_options(workspace, None, None).await
}

async fn build_wire_test_agent_with_options(
    workspace: &std::path::Path,
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

    Box::pin(ZedAgent::new(ZedAgentSetup {
        config: core_config,
        credential_storage_mode: AuthCredentialsStoreMode::default(),
        zed_config: AgentClientProtocolZedConfig::default(),
        tools_config: ToolsConfig::default(),
        commands_config: CommandsConfig::default(),
        custom_providers: &[],
        provider_timeouts: vtcode_config::TimeoutsConfig::default(),
        system_prompt: String::new(),
        title: Some("Wire test".to_string()),
        primary_agents,
        skip_confirmations: true,
        vt_config,
        audit_logger,
    }))
    .await
}

async fn large_compaction_session(agent: &ZedAgent, workspace: &std::path::Path) -> SessionHandle {
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

fn compaction_config(enabled: bool) -> VTCodeConfig {
    let mut config = VTCodeConfig::default();
    config.agent.harness.auto_compaction_enabled = enabled;
    config.agent.harness.auto_compaction_threshold_tokens = Some(1);
    config
}

struct FlakyProvider {
    attempts: AtomicUsize,
    network_failures: usize,
    invalid_request: bool,
}

struct PendingProvider;

struct BadGatewayProvider {
    attempts: AtomicUsize,
}

#[async_trait]
impl LLMProvider for PendingProvider {
    fn name(&self) -> &str {
        "timeout-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        std::future::pending().await
    }
}

#[async_trait]
impl LLMProvider for BadGatewayProvider {
    fn name(&self) -> &str {
        "bad-gateway-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
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
        let _previous_attempts = self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(LLMError::Provider {
            message: "OpenAI Chat Completions error (status 502 Bad Gateway)".to_string(),
            metadata: Some(LLMErrorMetadata::new(
                "openai",
                Some(502),
                None,
                None,
                None,
                None,
                Some("502 Bad Gateway".to_string()),
            )),
        })
    }
}

#[async_trait]
impl LLMProvider for FlakyProvider {
    fn name(&self) -> &str {
        "retry-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.invalid_request {
            return Err(LLMError::InvalidRequest { message: "bad request".to_string(), metadata: None });
        }
        if attempt < self.network_failures {
            return Err(LLMError::Network {
                message: "temporary disconnect".to_string(),
                metadata: None,
            });
        }
        Ok(LLMResponse::new("test-model", "recovered"))
    }
}

fn retry_runtime() -> ProviderRequestRuntime {
    let provider = CustomProviderConfig {
        name: "retry-test".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 2,
            retry_initial_backoff_ms: 1,
            retry_max_backoff_ms: 2,
            retry_jitter: false,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    super::super::super::provider_runtime::ProviderRuntimeRegistry::new(
        &[provider],
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider("retry-test")
}

fn timeout_runtime() -> ProviderRequestRuntime {
    let provider = CustomProviderConfig {
        name: "timeout-test".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 0,
            total_generation_timeout_seconds: 1,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    super::super::super::provider_runtime::ProviderRuntimeRegistry::new(
        &[provider],
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider("timeout-test")
}
