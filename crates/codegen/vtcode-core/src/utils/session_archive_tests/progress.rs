use super::*;

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_progress_persists_budget_and_recent_messages() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let full = vec![
        SessionMessage::new(MessageRole::User, "first"),
        SessionMessage::new(MessageRole::Assistant, "middle"),
        SessionMessage::new(MessageRole::User, "latest"),
    ];
    let recent = vec![SessionMessage::new(MessageRole::Assistant, "recent")];
    let diagnostics = SnapshotTurnDiagnostics {
        requested_tool_calls: 3,
        denied_tool_calls: 1,
        suppressed_tool_previews: 2,
        model_visible_tool_preview_budget_exhausted: true,
        ..Default::default()
    };

    let path = archive.persist_progress(SessionProgressArgs {
        total_messages: full.len(),
        distinct_tools: vec!["tool_a".to_owned()],
        messages: full.clone(),
        recent_messages: recent.clone(),
        turn_number: 2,
        token_usage: Some("10 tokens".to_string()),
        max_context_tokens: Some(128),
        loaded_skills: None,
        turn_diagnostics: Some(diagnostics.clone()),
    })?;

    let stored =
        fs::read_to_string(&path).with_context(|| format!("failed to read stored session: {}", path.display()))?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;

    assert_eq!(snapshot.messages, full);
    let progress = snapshot.progress.expect("progress should exist");
    assert_eq!(progress.turn_number, 2);
    assert_eq!(progress.recent_messages, recent);
    assert_eq!(progress.token_usage, Some("10 tokens".to_string()));
    assert_eq!(progress.tool_summaries, vec!["tool_a".to_string()]);
    assert_eq!(progress.max_context_tokens, Some(128));
    assert_eq!(progress.turn_diagnostics, Some(diagnostics));
    assert_eq!(snapshot.transcript, vec!["recent".to_string()]);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_progress_normalizes_exec_tool_aliases_in_summaries() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let recent = vec![SessionMessage::new(MessageRole::Assistant, "done")];

    let path = archive.persist_progress(SessionProgressArgs {
        total_messages: 1,
        distinct_tools: vec![
            tool_names::UNIFIED_EXEC.to_string(),
            tool_names::RUN_PTY_CMD.to_string(),
            tool_names::SEND_PTY_INPUT.to_string(),
            tool_names::READ_PTY_SESSION.to_string(),
            tool_names::LIST_PTY_SESSIONS.to_string(),
            tool_names::CLOSE_PTY_SESSION.to_string(),
            tool_names::EXECUTE_CODE.to_string(),
            tool_names::EXEC_COMMAND.to_string(),
            tool_names::WRITE_STDIN.to_string(),
            "shell".to_string(),
            "exec_pty_cmd".to_string(),
            "exec".to_string(),
            "container.exec".to_string(),
        ],
        messages: recent.clone(),
        recent_messages: recent,
        turn_number: 2,
        token_usage: Some("10 tokens".to_string()),
        max_context_tokens: Some(128),
        loaded_skills: None,
        turn_diagnostics: None,
    })?;

    let stored =
        fs::read_to_string(&path).with_context(|| format!("failed to read stored session: {}", path.display()))?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;

    assert_eq!(snapshot.distinct_tools, vec![tool_names::UNIFIED_EXEC.to_string()]);
    let progress = snapshot.progress.expect("progress should exist");
    assert_eq!(progress.tool_summaries, vec![tool_names::UNIFIED_EXEC.to_string()]);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn throttled_async_progress_does_not_claim_durable_checkpoint() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata = SessionArchiveMetadata::new("ThrottleWS", "/tmp/throttle", "model", "provider", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let initial_messages = vec![SessionMessage::new(MessageRole::User, "initial")];
    let first_status = archive
        .persist_progress_async_with_status(SessionProgressArgs {
            total_messages: initial_messages.len(),
            distinct_tools: Vec::new(),
            messages: initial_messages.clone(),
            recent_messages: initial_messages.clone(),
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;
    assert!(first_status.is_persisted());
    let stored_before = fs::read_to_string(archive.path()).context("read first progress checkpoint")?;

    let updated_messages = vec![
        SessionMessage::new(MessageRole::User, "initial"),
        SessionMessage::new(MessageRole::User, "new steering intent"),
    ];
    let second_status = archive
        .persist_progress_async_with_status(SessionProgressArgs {
            total_messages: updated_messages.len(),
            distinct_tools: Vec::new(),
            messages: updated_messages.clone(),
            recent_messages: updated_messages,
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;
    assert!(matches!(second_status, SessionProgressPersistenceStatus::Throttled(_)));

    let stored_after = fs::read_to_string(archive.path()).context("read throttled progress checkpoint")?;
    assert_eq!(stored_after, stored_before, "throttling must not acknowledge an unwritten message");
    let snapshot: SessionSnapshot = serde_json::from_str(&stored_after).context("parse progress checkpoint")?;
    assert_eq!(snapshot.messages, initial_messages);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn forced_async_progress_persists_latest_message_after_throttle() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata = SessionArchiveMetadata::new("ForcedWS", "/tmp/forced", "model", "provider", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let initial = vec![SessionMessage::new(MessageRole::User, "initial")];
    archive
        .persist_progress_async_with_status(SessionProgressArgs {
            total_messages: 1,
            distinct_tools: Vec::new(),
            messages: initial.clone(),
            recent_messages: initial.clone(),
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;

    let latest = vec![
        SessionMessage::new(MessageRole::User, "initial"),
        SessionMessage::new(MessageRole::Assistant, "blocked latest response"),
    ];
    let status = archive
        .persist_progress_async_with_status_forced(SessionProgressArgs {
            total_messages: latest.len(),
            distinct_tools: Vec::new(),
            messages: latest.clone(),
            recent_messages: latest,
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;

    assert!(status.is_persisted());
    let snapshot: SessionSnapshot = serde_json::from_str(&fs::read_to_string(archive.path())?)?;
    assert_eq!(snapshot.messages[1].content.as_text_borrowed(), Some("blocked latest response"));

    let identifier = archive
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("session archive has no identifier")?;
    let verified_identifier = archive
        .verify_persisted_resume_identifier(archive.path())
        .await?
        .context("forced checkpoint should produce a verified resume identifier")?;
    assert_eq!(verified_identifier.as_str(), identifier);

    let listing = find_session_by_identifier(identifier)
        .await?
        .context("forced checkpoint should be resolvable by its archive identifier")?;
    assert_eq!(listing.path, archive.path());
    assert_eq!(listing.snapshot.messages[1].content.as_text_borrowed(), Some("blocked latest response"));
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn durable_checkpoint_rewrites_history_within_the_same_turn() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata = SessionArchiveMetadata::new("ACP", "/tmp/acp", "model", "provider", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let initial_messages = vec![SessionMessage::new(MessageRole::User, "continue")];
    archive
        .persist_checkpoint_async(SessionProgressArgs {
            total_messages: initial_messages.len(),
            distinct_tools: Vec::new(),
            messages: initial_messages.clone(),
            recent_messages: initial_messages,
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;

    let completed_messages = vec![
        SessionMessage::new(MessageRole::User, "continue"),
        SessionMessage::new(MessageRole::Assistant, "resumed response"),
    ];
    let status = archive
        .persist_checkpoint_async(SessionProgressArgs {
            total_messages: completed_messages.len(),
            distinct_tools: Vec::new(),
            messages: completed_messages.clone(),
            recent_messages: completed_messages.clone(),
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;

    assert!(status.is_persisted());
    let stored = fs::read_to_string(archive.path()).context("read durable ACP checkpoint")?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("parse durable ACP checkpoint")?;
    assert_eq!(snapshot.messages, completed_messages);
    Ok(())
}
