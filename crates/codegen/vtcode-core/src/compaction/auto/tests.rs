//! Automatic compaction eligibility, suppression, and steering contracts.

use super::*;
use crate::compaction::{SUPPRESS_NONE, SUPPRESS_STICKY};
use crate::llm::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse, Message};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct FailingProvider {
    calls: AtomicUsize,
}

struct SuccessfulProvider;

#[async_trait]
impl LLMProvider for SuccessfulProvider {
    fn name(&self) -> &str {
        "successful"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("successful-model", "summary"))
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["successful-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        4_096
    }
}

#[async_trait]
impl LLMProvider for FailingProvider {
    fn name(&self) -> &str {
        "failing"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(LLMError::Provider {
            message: "context length exceeded".to_string(),
            metadata: None,
        })
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["failing-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1000
    }
}

/// Shared automatic input for suppression and deterministic provider failure.
struct FailureScenario {
    provider: FailingProvider,
    config: VTCodeConfig,
    history: Vec<Message>,
    suppressed: u8,
}

impl FailureScenario {
    fn new(suppressed: u8) -> Self {
        let mut config = VTCodeConfig::default();
        config.agent.harness.auto_compaction_enabled = true;
        Self {
            provider: FailingProvider::default(),
            config,
            history: vec![Message::user("hello".to_string())],
            suppressed,
        }
    }

    async fn run(&mut self, engine_cfg: CompactionConfig) -> Result<Option<AutoCompactionOutcome>> {
        auto_compact_messages(
            AutoCompactionInput {
                provider: &self.provider,
                model: "failing-model",
                session_id: "s1",
                workspace_root: Path::new("."),
                vt_cfg: Some(&self.config),
                current_token_usage: 900,
                touched_files: &[],
                engine_cfg,
                manual_options: ManualCompactionOptions::default(),
                placement: MemoryEnvelopePlacement::BeforeLastUserOrSummary,
                prefire: None,
                auto_compact_suppressed: &mut self.suppressed,
                force_compaction: false,
                steering_update: None,
            },
            &mut self.history,
        )
        .await
    }

    fn assert_no_work_keeps_sticky_state(&self, original: &[Message]) {
        assert_eq!(self.suppressed, SUPPRESS_STICKY);
        assert_eq!(self.history.as_slice(), original);
        assert_eq!(self.provider.calls.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn suppressed_state_skips_compaction() {
    let mut scenario = FailureScenario::new(SUPPRESS_STICKY);
    let original = scenario.history.clone();

    let result = scenario.run(CompactionConfig::default()).await;

    assert!(result.expect("suppression skips compaction").is_none());
    scenario.assert_no_work_keeps_sticky_state(&original);
}

#[tokio::test]
async fn failure_sets_suppression_on_deterministic_error() {
    let mut scenario = FailureScenario::new(SUPPRESS_NONE);

    let result = scenario
        .run(CompactionConfig {
            keep_last_messages: 0,
            ..CompactionConfig::default()
        })
        .await;

    assert!(result.is_err());
    assert_eq!(scenario.suppressed, SUPPRESS_STICKY);
    assert_eq!(scenario.provider.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn automatic_compaction_persists_live_steering_snapshot() {
    let provider = SuccessfulProvider;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.agent.harness.auto_compaction_enabled = true;
    vt_cfg.context.dynamic.enabled = true;
    vt_cfg.context.dynamic.persist_history = true;
    let workspace = tempfile::tempdir().expect("tempdir");
    let mut history = (0..12)
        .map(|index| Message::user(format!("request {index}")))
        .collect::<Vec<_>>();
    let intent = crate::core::agent::steering::QueuedFollowUpIntent::from_parts("intent-1", "finish the request");
    let steering_update = SessionMemoryEnvelopeUpdate {
        pending_intents: Some(vec![intent.clone()]),
        applied_intent_ids: vec!["applied-1".to_string()],
        ..Default::default()
    };
    let mut suppressed = SUPPRESS_NONE;

    let result = auto_compact_messages(
        AutoCompactionInput {
            provider: &provider,
            model: "successful-model",
            session_id: "session-1",
            workspace_root: workspace.path(),
            vt_cfg: Some(&vt_cfg),
            current_token_usage: 4_000,
            touched_files: &[],
            engine_cfg: CompactionConfig {
                keep_last_messages: 0,
                ..CompactionConfig::default()
            },
            manual_options: ManualCompactionOptions::default(),
            placement: MemoryEnvelopePlacement::BeforeLastUserOrSummary,
            prefire: None,
            auto_compact_suppressed: &mut suppressed,
            force_compaction: false,
            steering_update: Some(&steering_update),
        },
        &mut history,
    )
    .await
    .expect("automatic compaction should succeed")
    .expect("automatic compaction should run");

    let envelope = result.envelope.expect("compaction should produce an envelope");
    assert_eq!(envelope.pending_intents, vec![intent]);
    assert_eq!(envelope.applied_intent_ids, vec!["applied-1"]);
}
