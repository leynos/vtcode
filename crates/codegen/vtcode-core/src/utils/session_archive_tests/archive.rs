use super::*;

async fn persisted_snapshot_fixture()
-> Result<(SessionSnapshot, SessionArchiveMetadata, Vec<String>, Vec<SessionMessage>)> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium")
            .with_external_thread_id("thread-123");
    let mut archive_metadata = metadata.clone();
    archive_metadata.client_metadata = Some(serde_json::Map::new());
    let archive = SessionArchive::new(archive_metadata.clone(), None).await?;
    let transcript = vec!["line one".to_owned(), "line two".to_owned()];
    let messages = vec![
        SessionMessage::new(MessageRole::User, "Hello world"),
        SessionMessage::new(MessageRole::Assistant, "Hi there"),
    ];
    let path = archive.finalize(transcript.clone(), 4, vec!["tool_a".to_owned()], messages.clone())?;
    let stored =
        fs::read_to_string(&path).with_context(|| format!("failed to read stored session: {}", path.display()))?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;

    Ok((snapshot, archive_metadata, transcript, messages))
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_archive_persists_snapshot_metadata_and_counts() -> Result<()> {
    let (snapshot, metadata, _, _) = persisted_snapshot_fixture().await?;

    assert_eq!(snapshot.metadata, metadata);
    assert_eq!(snapshot.total_messages, 4);
    assert_eq!(snapshot.distinct_tools, vec!["tool_a".to_owned()]);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_archive_persists_snapshot_transcript_and_messages() -> Result<()> {
    let (snapshot, _, transcript, messages) = persisted_snapshot_fixture().await?;

    assert_eq!(snapshot.transcript, transcript);
    assert_eq!(snapshot.messages, messages);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_archive_finalization_preserves_last_turn_diagnostics() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let diagnostics = SnapshotTurnDiagnostics {
        elapsed_ms: 321,
        requested_tool_calls: 4,
        admitted_tool_calls: 2,
        unadmitted_tool_calls: 2,
        ..Default::default()
    };

    let path = archive.finalize_with_diagnostics(Vec::new(), 0, Vec::new(), Vec::new(), Some(diagnostics.clone()))?;
    let stored = fs::read_to_string(&path).context("failed to read stored session")?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;

    assert_eq!(
        snapshot
            .progress
            .as_deref()
            .and_then(|progress| progress.turn_diagnostics.as_ref()),
        Some(&diagnostics)
    );
    assert!(
        snapshot
            .progress
            .as_deref()
            .is_some_and(|progress| progress.recent_messages.is_empty())
    );
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_archive_persists_budget_limit_continuation_metadata() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium")
            .with_continuation_metadata(Some(SessionContinuationMetadata::budget_limit(2.5, 2.7, true)));
    let archive = SessionArchive::new(metadata.clone(), None).await?;

    let path = archive.finalize(Vec::new(), 0, Vec::new(), Vec::new())?;
    let stored =
        fs::read_to_string(&path).with_context(|| format!("failed to read stored session: {}", path.display()))?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;

    let continuation = snapshot
        .metadata
        .budget_limit_continuation()
        .expect("budget limit continuation metadata should exist");
    assert_eq!(continuation.max_budget_usd(), Some(2.5));
    assert_eq!(continuation.actual_cost_usd(), Some(2.7));
    assert!(continuation.summary_available);
    assert_eq!(snapshot.metadata, metadata);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_archive_path_collision_adds_suffix() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium");

    let started_at = Utc
        .with_ymd_and_hms(2025, 9, 25, 10, 15, 30)
        .unwrap()
        .with_nanosecond(123_456_000)
        .unwrap();

    let first_path = generate_unique_archive_path(temp_dir.path(), &metadata, started_at, None);
    fs::write(&first_path, "{}").context("failed to create sentinel file")?;

    let second_path = generate_unique_archive_path(temp_dir.path(), &metadata, started_at, None);

    assert_ne!(first_path, second_path);
    let second_name = second_path.file_name().and_then(|name| name.to_str()).expect("file name");
    assert!(second_name.contains("-01"));

    Ok(())
}

#[test]
fn session_archive_filename_includes_microseconds_and_pid() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium");

    let started_at = Utc
        .with_ymd_and_hms(2025, 9, 25, 10, 15, 30)
        .unwrap()
        .with_nanosecond(654_321_000)
        .expect("nanosecond set");

    let path = generate_unique_archive_path(temp_dir.path(), &metadata, started_at, None);
    let name = path.file_name().and_then(|value| value.to_str()).expect("file name string");

    assert!(name.contains("20250925T101530Z_654321"));
    let pid_fragment = format!("{:05}", process::id());
    assert!(name.contains(&pid_fragment));

    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn reserve_session_identifier_can_be_reused_for_archive() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let session_id = reserve_session_archive_identifier("ExampleWorkspace", None).await?;
    assert!(session_id.starts_with("session-exampleworkspace-"));

    let metadata =
        SessionArchiveMetadata::new("ExampleWorkspace", "/tmp/example", "model-x", "provider-y", "dark", "medium")
            .with_debug_log_path(Some("/tmp/debug-session.log".to_string()));
    let archive = SessionArchive::new_with_identifier(metadata.clone(), session_id.clone())
        .await
        .context("failed to create archive with reserved session id")?;
    let path = archive.finalize(
        vec!["line one".to_owned()],
        1,
        vec![],
        vec![SessionMessage::new(MessageRole::User, "hello")],
    )?;
    let stored =
        fs::read_to_string(&path).with_context(|| format!("failed to read stored session: {}", path.display()))?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;

    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("missing file stem"))?;
    assert_eq!(stem, session_id);
    assert_eq!(snapshot.metadata.debug_log_path, Some("/tmp/debug-session.log".to_string()));

    Ok(())
}

#[test]
fn generated_session_identifier_includes_workspace_and_custom_suffix() {
    let generated = generate_session_archive_identifier("Example Workspace", Some("branch".to_string()));

    assert!(generated.starts_with("session-example-workspace-"));
    assert!(generated.ends_with("-branch"));
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn resume_from_listing_reuses_existing_archive_path() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let session_id = reserve_session_archive_identifier("ResumeWorkspace", None).await?;
    let metadata = SessionArchiveMetadata::new(
        "ResumeWorkspace",
        "/tmp/resume-workspace",
        "model-a",
        "provider-a",
        "light",
        "medium",
    );
    let archive = SessionArchive::new_with_identifier(metadata.clone(), session_id.clone())
        .await
        .context("failed to create initial archive")?;
    let original_path = archive.finalize(
        vec!["user: first".to_string()],
        1,
        vec!["read_file".to_string()],
        vec![SessionMessage::new(MessageRole::User, "first")],
    )?;

    let listing = find_session_by_identifier(&session_id)
        .await?
        .context("expected archived session listing")?;
    let resumed = SessionArchive::resume_from_listing(&listing, metadata);
    let resumed_path = resumed.finalize(
        vec!["user: second".to_string()],
        2,
        vec!["read_file".to_string()],
        vec![
            SessionMessage::new(MessageRole::User, "first"),
            SessionMessage::new(MessageRole::Assistant, "second"),
        ],
    )?;

    assert_eq!(resumed_path, original_path);
    let stored = fs::read_to_string(&resumed_path)
        .with_context(|| format!("failed to read stored session: {}", resumed_path.display()))?;
    let snapshot: SessionSnapshot = serde_json::from_str(&stored).context("failed to deserialize stored snapshot")?;
    assert_eq!(snapshot.total_messages, 2);
    assert_eq!(snapshot.messages.len(), 2);

    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn session_archive_skips_writes_when_history_persistence_is_disabled() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::None, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata =
        SessionArchiveMetadata::new("NoHistory", "/tmp/no-history", "model-x", "provider-y", "dark", "medium");
    let archive = SessionArchive::new(metadata, None).await?;
    let path = archive.finalize(
        vec!["line one".to_owned()],
        1,
        Vec::new(),
        vec![SessionMessage::new(MessageRole::User, "hello")],
    )?;

    assert_eq!(path, archive.path());
    assert!(!path.exists(), "history disabled should not write archive files");

    let status = archive
        .persist_progress_async_with_status_forced(SessionProgressArgs {
            total_messages: 1,
            distinct_tools: Vec::new(),
            messages: vec![SessionMessage::new(MessageRole::User, "not persisted")],
            recent_messages: vec![SessionMessage::new(MessageRole::User, "not persisted")],
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: None,
            turn_diagnostics: None,
        })
        .await?;
    assert!(matches!(status, SessionProgressPersistenceStatus::Disabled(_)));
    assert!(!archive.path().exists());
    Ok(())
}
