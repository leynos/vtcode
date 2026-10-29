//! Refused automatic compaction must preserve live history and durable checkpoint bytes.

use super::archive::archived_listing;
use super::*;
use crate::zed::agent::compaction::CompactionRequest;
use crate::zed::provider_runtime::ProviderRuntimeRegistry;
use assert_fs::TempDir;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::provider::ToolCall;
use vtcode_core::llm::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse};

struct SummaryProvider {
    requests: Mutex<Vec<LLMRequest>>,
}

#[async_trait::async_trait]
impl LLMProvider for SummaryProvider {
    fn name(&self) -> &str {
        "admission-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        32_768
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.requests.lock().expect("record summary request").push(request);
        Ok(LLMResponse::new("test", "Inspected active work; preserve all acceptance criteria."))
    }
}

struct RejectedCompactionFixture {
    agent: ZedAgent,
    handle: SessionHandle,
    thread: ThreadRuntimeHandle,
    checkpoint_path: std::path::PathBuf,
    original_checkpoint: Vec<u8>,
    original_history: Vec<Message>,
    original_prompt_tokens: usize,
}

impl RejectedCompactionFixture {
    async fn new(workspace: &Path) -> Self {
        let mut config = VTCodeConfig::default();
        config.agent.harness.auto_compaction_enabled = true;
        config.agent.harness.auto_compaction_threshold_tokens = Some(1);
        let mut agent = build_agent_with_vt_config(workspace, "duck", Some(&config)).await;
        agent.system_prompt = "Preserve the complete task and current workspace constraints.".to_string();
        let task = "Acceptance criterion: preserve requested behavior.\n".repeat(2_200);
        let task_tokens = Message::user(task.clone()).estimate_tokens();
        // Core admits the protected task within 28,160 tokens, but ACP reserves
        // provider output and must reject it at its 15,360-token prompt budget.
        assert!(task_tokens > 15_360 && task_tokens < 28_160);
        let listing = archived_listing("rejected-compaction", workspace, &task);
        let checkpoint_path = listing.path.clone();
        let archive = SessionArchive::resume_from_listing(&listing, listing.snapshot.metadata.clone());
        let thread = agent
            .thread_manager
            .start_thread_with_identifier("rejected-compaction", ThreadBootstrap::from_listing(listing));
        let handle = agent.build_session_handle(
            thread.clone(),
            SessionHandleBootstrap {
                subagent_controller: None,
                session_id: acp::SessionId::new("rejected-compaction"),
                archive: Some(archive),
                trigger: SessionStartTrigger::Resume,
                workspace_runtime: None,
                workspace_lease: None,
            },
        );
        populate_correlated_admission_work(&agent, &handle);
        agent.checkpoint_session(&handle).await.expect("write original checkpoint");
        let original_checkpoint = fs::read(&checkpoint_path).expect("read original checkpoint");
        let original_history = thread.messages();
        let original_prompt_tokens = agent
            .resolved_messages(&handle)
            .expect("resolve original prompt")
            .iter()
            .map(Message::estimate_tokens)
            .sum();
        Self {
            agent,
            handle,
            thread,
            checkpoint_path,
            original_checkpoint,
            original_history,
            original_prompt_tokens,
        }
    }

    fn assert_refused_admission(&self, error: &str) {
        let prefix =
            format!("ACP compaction reduced the prompt from an estimated {} to ", self.original_prompt_tokens,);
        let compacted_tokens = error
            .strip_prefix(&prefix)
            .and_then(|message| message.strip_suffix(" tokens, which still exceeds its safe provider admission budget"))
            .and_then(|tokens| tokens.parse::<usize>().ok())
            .expect("exact ACP candidate-admission error with estimated token counts");
        assert!(compacted_tokens >= 15_360);
    }

    fn assert_history_and_checkpoint_unchanged(&self) {
        assert_eq!(self.thread.messages(), self.original_history, "refusal must not replace live history");
        assert_eq!(
            fs::read(&self.checkpoint_path).expect("read checkpoint after refusal"),
            self.original_checkpoint,
            "refusal must not publish the candidate checkpoint"
        );
    }
}

fn populate_correlated_admission_work(agent: &ZedAgent, handle: &SessionHandle) {
    // Exceed the summary short-circuit using actual correlated work.
    for step in 0..12 {
        let call_id = format!("admission-read-{step}");
        agent
            .push_message(
                handle,
                Message::assistant_with_tools(
                    "Inspecting acceptance criteria".to_string(),
                    vec![ToolCall::function(
                        call_id.clone(),
                        "read_file".to_string(),
                        "{}".to_string(),
                    )],
                ),
            )
            .expect("append correlated assistant work");
        agent
            .push_message(
                handle,
                Message::tool_response(
                    call_id,
                    format!("ACTIVE_ADMISSION_WORK: verified criterion {step}. {}", "checked boundary ".repeat(100)),
                ),
            )
            .expect("append correlated tool work");
    }
}

fn assert_summarized_active_work(provider: &SummaryProvider) {
    let requests = provider.requests.lock().expect("summary requests");
    assert_eq!(requests.len(), 1, "the real local summarizer must run before refusal");
    assert!(requests.iter().any(|request| {
        request
            .messages
            .iter()
            .any(|message| message.content.as_text().contains("ACTIVE_ADMISSION_WORK"))
    }));
}

#[tokio::test]
async fn rejected_compaction_preserves_live_history_and_checkpoint() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("admission test workspace");
    let fixture = Box::pin(RejectedCompactionFixture::new(temp.path())).await;
    let provider = SummaryProvider { requests: Mutex::new(Vec::new()) };
    let runtime = ProviderRuntimeRegistry::new(
        &[],
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider("admission-test");
    let _turn_id = fixture.thread.begin_turn().expect("exclusive compaction turn");
    let error = fixture
        .agent
        .maybe_compact_session(CompactionRequest {
            session: &fixture.handle,
            provider: &provider,
            runtime: &runtime,
            model: "test",
            tools: None,
        })
        .await
        .expect_err("candidate must exceed ACP admission budget")
        .to_string();
    fixture.thread.finish_turn();

    fixture.assert_refused_admission(&error);
    assert_summarized_active_work(&provider);
    fixture.assert_history_and_checkpoint_unchanged();
}
