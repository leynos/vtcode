//! Tests archive lookup, ordering, search, previews, and handling of unreadable entries.
use super::*;
use std::fmt::Write as FmtWrite;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context as TracingContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct ArchiveDiagnosticCapture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for ArchiveDiagnosticCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: TracingContext<'_, S>) {
        let mut fields = CapturedArchiveFields::default();
        event.record(&mut fields);
        self.0.lock().expect("archive diagnostic capture lock").push(fields.0);
    }
}

#[derive(Default)]
struct CapturedArchiveFields(String);

impl Visit for CapturedArchiveFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write archive trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write archive trace field");
    }
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn find_session_by_identifier_returns_match() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata = SessionArchiveMetadata::new("Sample", "/tmp/sample", "model-x", "provider-y", "dark", "medium");
    let archive = SessionArchive::new(metadata.clone(), None).await?;
    let messages = vec![
        SessionMessage::new(MessageRole::User, "Hi"),
        SessionMessage::new(MessageRole::Assistant, "Hello"),
    ];
    let path = archive.finalize(Vec::new(), messages.len(), Vec::new(), messages)?;
    let identifier = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("missing file stem"))?
        .to_string();

    let listing = find_session_by_identifier(&identifier)
        .await?
        .ok_or_else(|| anyhow!("expected session to be found"))?;
    assert_eq!(listing.identifier(), identifier);
    assert_eq!(listing.snapshot.metadata, metadata);

    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn list_recent_sessions_orders_entries() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let first_metadata = SessionArchiveMetadata::new("First", "/tmp/first", "model-a", "provider-a", "light", "medium");
    let first_archive = SessionArchive::new(first_metadata.clone(), None).await?;
    first_archive.finalize(
        vec!["first".to_owned()],
        1,
        Vec::new(),
        vec![SessionMessage::new(MessageRole::User, "First")],
    )?;

    tokio::time::sleep(Duration::from_millis(10)).await;

    let second_metadata = SessionArchiveMetadata::new("Second", "/tmp/second", "model-b", "provider-b", "dark", "high");
    let second_archive = SessionArchive::new(second_metadata.clone(), None).await?;
    second_archive.finalize(
        vec!["second".to_owned()],
        2,
        vec!["tool_b".to_owned()],
        vec![SessionMessage::new(MessageRole::User, "Second")],
    )?;

    let listings = list_recent_sessions(10).await?;
    assert_eq!(listings.len(), 2);
    assert_eq!(listings[0].snapshot.metadata, second_metadata);
    assert_eq!(listings[1].snapshot.metadata, first_metadata);
    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn list_recent_sessions_propagates_directory_resolution_errors() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let blocked_path = temp_dir.path().join("sessions-is-a-file");
    fs::write(&blocked_path, "not a directory")
        .with_context(|| format!("failed to create blocking file {}", blocked_path.display()))?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, &blocked_path);

    let error = list_recent_sessions(10)
        .await
        .expect_err("a session path blocked by a regular file must fail");
    let error = format!("{error:#}");
    assert!(error.contains("failed to resolve session directory for listing"));
    assert!(error.contains("session_directory_resolution_failed"));
    assert!(!error.contains(&blocked_path.display().to_string()));

    Ok(())
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn list_recent_sessions_skips_unreadable_archive_files() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let sessions_dir = temp_dir.path().join("private-session-listing-path");
    fs::create_dir(&sessions_dir).context("failed to create session directory")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, &sessions_dir);
    let valid_metadata = SessionArchiveMetadata::new("Valid", "/tmp/valid", "model-a", "provider-a", "light", "medium");
    let valid_archive = SessionArchive::new(valid_metadata.clone(), None).await?;
    valid_archive.finalize(
        vec!["valid".to_owned()],
        1,
        Vec::new(),
        vec![SessionMessage::new(MessageRole::User, "Valid")],
    )?;

    let archive_path = sessions_dir.join("private-session-listing-archive.json");
    fs::write(&archive_path, "{ malformed json")
        .with_context(|| format!("failed to write malformed archive {}", archive_path.display()))?;
    fs::write(sessions_dir.join("debug-old-session.log"), "debug output")
        .context("failed to write unrelated session debug log")?;

    let capture = ArchiveDiagnosticCapture::default();
    let captured_events = Arc::clone(&capture.0);
    let subscriber = Registry::default().with(capture);
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let listings = list_recent_sessions(10).await?;
    drop(_subscriber_guard);
    assert_eq!(listings.len(), 1);
    assert_eq!(listings[0].snapshot.metadata, valid_metadata);

    let captured = captured_events.lock().expect("archive diagnostic capture lock").join("\n");
    assert!(!captured.contains(&sessions_dir.display().to_string()));
    assert!(!captured.contains(&archive_path.display().to_string()));
    assert!(captured.contains("operation=\"session_archive_listing\""));
    assert!(captured.contains("outcome=\"archive_skipped\""));
    assert!(captured.contains("error_category=\"session_archive_read_failed\""));

    let lookup_error = find_session_by_identifier("private-session-listing-archive")
        .await
        .expect_err("malformed archive lookup must return an error");
    let lookup_error = format!("{lookup_error:#}");
    assert!(lookup_error.contains("session_archive_read_failed"));
    assert!(!lookup_error.contains(&sessions_dir.display().to_string()));
    assert!(!lookup_error.contains(&archive_path.display().to_string()));

    Ok(())
}

#[test]
fn listing_previews_return_first_non_empty_lines() {
    let metadata = SessionArchiveMetadata::new("Workspace", "/tmp/ws", "model", "provider", "dark", "medium");
    let long_response = "response snippet ".repeat(6);
    let snapshot = SessionSnapshot {
        metadata,
        started_at: Utc::now(),
        ended_at: Utc::now(),
        total_messages: 2,
        distinct_tools: Vec::new(),
        transcript: Vec::new(),
        messages: vec![
            SessionMessage::new(MessageRole::System, ""),
            SessionMessage::new(MessageRole::User, "  prompt line\nsecond"),
            SessionMessage::new(MessageRole::Assistant, long_response.clone()),
        ],
        progress: None,
        error_logs: Vec::new(),
    };
    let listing = SessionListing {
        path: PathBuf::from("session-workspace.json"),
        snapshot,
    };

    assert_eq!(listing.first_prompt_preview(), Some("prompt line".to_owned()));
    let expected = truncate_preview(&long_response, 80);
    assert_eq!(listing.first_reply_preview(), Some(expected));
}

#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn search_sessions_finds_keyword_case_insensitively() -> Result<()> {
    let _settings_lock = lock_history_test_guard().await;
    let _history_guard = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

    let metadata = SessionArchiveMetadata::new("SearchWS", "/tmp/s", "mod", "prov", "d", "m");
    let archive = SessionArchive::new(metadata, None).await?;

    let messages = vec![
        SessionMessage::new(MessageRole::User, "Where is the secret API key?"),
        SessionMessage::new(MessageRole::Assistant, "The secret key is defined in .env.local"),
    ];
    archive.finalize(vec![], 2, vec![], messages)?;

    let results = search_sessions("SECRET KEY", 10, 5).await?;
    assert!(!results.is_empty());
    assert!(results[0].content_snippet.to_lowercase().contains("secret key"));
    assert_eq!(results[0].role, MessageRole::Assistant);

    Ok(())
}
