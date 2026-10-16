//! Verifies archive retention removes oldest files when count or total-size limits are exceeded.
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

#[test]
fn session_archive_retention_prunes_oldest_by_count() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    for idx in 0..3 {
        let path = temp_dir.path().join(format!("session-{idx}.json"));
        fs::write(&path, format!("{{\"idx\":{idx}}}"))
            .with_context(|| format!("failed to write {}", path.display()))?;
        std::thread::sleep(Duration::from_millis(5));
    }

    apply_session_retention_with_limits(
        temp_dir.path(),
        SessionRetentionLimits {
            max_files: 2,
            max_age_days: 365,
            max_total_size_bytes: 10 * BYTES_PER_MB,
        },
    )?;

    let mut remaining = fs::read_dir(temp_dir.path())
        .context("failed to list retained session files")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|path| path.file_name().and_then(|name| name.to_str()).map(|name| name.to_string()))
        .collect::<Vec<_>>();
    remaining.sort();

    assert_eq!(remaining.len(), 2);
    assert!(!remaining.iter().any(|name| name == "session-0.json"));
    Ok(())
}

#[test]
fn session_archive_retention_prunes_by_total_size() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    for idx in 0..2 {
        let path = temp_dir.path().join(format!("session-{idx}.json"));
        fs::write(&path, "x".repeat(800_000)).with_context(|| format!("failed to write {}", path.display()))?;
        std::thread::sleep(Duration::from_millis(5));
    }

    apply_session_retention_with_limits(
        temp_dir.path(),
        SessionRetentionLimits {
            max_files: 10,
            max_age_days: 365,
            max_total_size_bytes: BYTES_PER_MB,
        },
    )?;

    let remaining = fs::read_dir(temp_dir.path())
        .context("failed to list retained session files")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();

    assert_eq!(remaining.len(), 1);
    let remaining_name = remaining[0].file_name().and_then(|name| name.to_str()).unwrap_or_default();
    assert_eq!(remaining_name, "session-1.json");
    Ok(())
}

#[test]
fn session_archive_retention_keeps_sensitive_paths_out_of_logs_and_errors() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let sessions_dir = temp_dir.path().join("private-session-retention-path");
    fs::write(&sessions_dir, "not a directory").context("failed to create retention blocking file")?;

    let error = apply_session_retention_with_limits(
        &sessions_dir,
        SessionRetentionLimits {
            max_files: 10,
            max_age_days: 365,
            max_total_size_bytes: BYTES_PER_MB,
        },
    )
    .expect_err("a regular file cannot be read as a session directory");
    let error = format!("{error:#}");
    assert!(error.contains("session_directory_read_failed"));
    assert!(!error.contains(&sessions_dir.display().to_string()));

    let capture = ArchiveDiagnosticCapture::default();
    let captured_events = Arc::clone(&capture.0);
    let subscriber = Registry::default().with(capture);
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    apply_session_retention_best_effort(&sessions_dir);
    drop(_subscriber_guard);

    let captured = captured_events.lock().expect("archive diagnostic capture lock").join("\n");
    assert!(!captured.contains(&sessions_dir.display().to_string()));
    assert!(captured.contains("operation=\"session_archive_retention\""));
    assert!(captured.contains("outcome=\"failed\""));
    assert!(captured.contains("error_category=\"session_directory_read_failed\""));
    Ok(())
}
