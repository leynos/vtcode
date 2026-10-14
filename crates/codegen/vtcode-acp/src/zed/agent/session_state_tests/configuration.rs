//! Configuration behavior for ACP session state.
use super::*;
use assert_fs::TempDir;
use std::fs;

#[tokio::test]
async fn build_session_handle_falls_back_for_unknown_metadata_primary_agent() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let listing = session_listing(
        "session-vtcode-acp-archive",
        temp.path(),
        SessionArchiveMetadata::new("vtcode", temp.path().to_string_lossy(), "gpt-5.6-sol", "openai", "test", "low")
            .with_primary_agent("missing"),
        Vec::new(),
    );
    let thread = agent
        .thread_manager
        .start_thread_with_identifier("session-vtcode-acp-archive", ThreadBootstrap::from_listing(listing));

    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            session_id: acp::SessionId::new("session-1"),
            archive: None,
            trigger: SessionStartTrigger::NewSession,
            workspace_runtime: None,
        },
    );

    assert_eq!(primary_agent(&handle), "duck");
}

#[tokio::test]
async fn register_session_falls_back_for_unknown_default_primary_agent() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent_with_default_primary_agent(temp.path(), "research").await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    // "research" is not in the discovered specs, so the resolver falls
    // back to the built-in "build" agent.
    assert_eq!(primary_agent(&session), "build");
}

#[tokio::test]
async fn register_session_uses_known_default_primary_agent_ids() {
    for primary_agent in ["duck", "plan", "build", "auto"] {
        let temp = TempDir::new().unwrap();
        let agent = build_agent_with_default_primary_agent(temp.path(), primary_agent).await;
        let session_id = agent.register_session().unwrap();
        let session = agent.session_handle(&session_id).unwrap().unwrap();

        assert_eq!(self::primary_agent(&session), primary_agent);
    }
}

#[tokio::test]
async fn register_session_uses_custom_default_primary_agent() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/research.md"),
        r#"---
name: research
description: Research primary
mode: primary
permissions:
  default: deny
---
Research primary prompt."#,
    )
    .unwrap();

    let agent = build_agent_with_default_primary_agent(temp.path(), "research").await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    assert_eq!(primary_agent(&session), "research");
}

#[tokio::test]
async fn update_session_primary_agent_updates_session_data() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    assert!(agent.update_session_primary_agent(&session, "build".to_string()).unwrap());
    assert_eq!(primary_agent(&session), "build");
    assert_eq!(
        session
            .data
            .lock()
            .ok()
            .and_then(|data| data.thread.metadata())
            .and_then(|metadata| metadata.primary_agent)
            .as_deref(),
        Some("build")
    );
}

#[tokio::test]
async fn session_registry_reports_poisoned_lock() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();

    let poisoning_panicked = catch_unwind(AssertUnwindSafe(|| {
        let _guard = agent.sessions.lock().unwrap();
        panic!("poison ACP session registry for regression test");
    }))
    .is_err();
    let session_lookup_rejected = agent.session_handle(&session_id).is_err();

    assert!(
        poisoning_panicked && session_lookup_rejected,
        "poisoning panicked={poisoning_panicked}, session lookup rejected={session_lookup_rejected}"
    );
}

#[tokio::test]
async fn session_optional_state_accessors_preserve_absent_state() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    assert!(session.workspace_runtime().unwrap().is_none());
    assert!(session.lifecycle_hooks().unwrap().is_none());
    assert!(!session.has_stop_hooks().unwrap());
}

#[tokio::test]
async fn session_optional_state_accessors_report_poisoned_lock() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    let poisoning_panicked = catch_unwind(AssertUnwindSafe(|| {
        let _guard = session.data.lock().unwrap();
        panic!("poison ACP session state for regression test");
    }))
    .is_err();
    let operation_errors = [
        ("workspace runtime", session.workspace_runtime().is_err()),
        ("lifecycle hooks", session.lifecycle_hooks().is_err()),
        ("stop hooks", session.has_stop_hooks().is_err()),
        ("config options", agent.current_session_config_options(&session).is_err()),
        (
            "reasoning effort update",
            agent
                .update_session_reasoning_effort(&session, ReasoningEffortLevel::High)
                .is_err(),
        ),
        (
            "provider and model update",
            agent
                .update_session_provider_and_model(&session, "openai".to_string(), "gpt-4o".to_string())
                .is_err(),
        ),
        (
            "message push",
            agent
                .push_message(&session, Message::user("must not be dropped".to_string()))
                .is_err(),
        ),
        ("transcript path update", session.update_transcript_path().await.is_err()),
        ("session start hooks", agent.run_session_start_hooks(&session).await.is_err()),
        ("subagent controller", agent.session_subagent_controller(&session).is_err()),
        ("primary agent update", agent.update_session_primary_agent(&session, "build".to_string()).is_err()),
        ("resolved messages", agent.resolved_messages(&session).is_err()),
        ("task lifecycle forwarder", agent.ensure_task_lifecycle_forwarder(&session).is_err()),
        ("session end hooks", agent.run_session_end_hooks(SessionEndReason::Exit).await.is_err()),
    ];
    let successful_operations = operation_errors
        .into_iter()
        .filter_map(|(operation, rejected)| (!rejected).then_some(operation))
        .collect::<Vec<_>>();

    assert!(
        poisoning_panicked && successful_operations.is_empty(),
        "poisoning panicked={poisoning_panicked}, operations unexpectedly succeeded={successful_operations:?}"
    );
}

#[tokio::test]
async fn update_session_reasoning_effort_syncs_thread_metadata() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    assert!(
        agent
            .update_session_reasoning_effort(&session, ReasoningEffortLevel::High)
            .unwrap()
    );
    assert_eq!(
        session
            .data
            .lock()
            .ok()
            .and_then(|data| data.thread.metadata())
            .map(|metadata| metadata.reasoning_effort)
            .as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn current_session_config_options_omit_thought_level_when_model_lacks_support() {
    let temp = TempDir::new().unwrap();
    let mut agent = build_agent(temp.path()).await;
    agent.config.model = "claude-haiku-3".to_string();
    agent.config.provider = "anthropic".to_string();
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    let config_options = agent.current_session_config_options(&session).unwrap();

    assert_eq!(config_options.len(), 3);
    assert_eq!(config_options[0].id, acp::SessionConfigId::new("primary_agent"));
}
