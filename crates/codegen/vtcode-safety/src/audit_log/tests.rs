//! Regression coverage for audit sinks, durability, and safe diagnostics.

use super::sinks::JsonlFileState;
use super::*;
use std::fmt::Write as _;
use std::io::BufWriter;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

struct CapturedEvents {
    events: Arc<Mutex<Vec<String>>>,
    next_span_id: AtomicU64,
}

impl Subscriber for CapturedEvents {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = String::new();
        event.record(&mut CapturedFields(&mut fields));
        self.events
            .lock()
            .expect("captured event lock should be available")
            .push(fields);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct CapturedFields<'a>(&'a mut String);

impl Visit for CapturedFields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ignored = writeln!(self.0, "{}={value:?}", field.name());
    }
}

fn sample_entry(suffix: &str) -> ToolAuditEntry {
    ToolAuditEntry {
        timestamp_unix_ms: 1_700_000_000_000 + u64::from(suffix.bytes().next().unwrap_or(b'a')),
        session_id: format!("session-{suffix}"),
        turn_id: format!("turn-{suffix}"),
        tool_call_id: format!("call-{suffix}"),
        tool_name: "mcp::fetch::fetch".to_owned(),
        arguments_hash: sha256_hex(suffix.as_bytes()),
        arguments_redacted: None,
        result_hash: sha256_hex(format!("result-{suffix}").as_bytes()),
        result_summary: Some(format!("first line of result {suffix}")),
        duration_ms: 42,
        status: ToolAuditStatus::Success,
        sandbox_policy: None,
        transport: Some("stdio".to_owned()),
        server_address: None,
        server_port: None,
        model_id: Some("test-model".to_owned()),
        prompt_injection_flagged: false,
        reason: None,
    }
}

#[test]
fn in_memory_sink_records_entries() {
    let sink = InMemorySink::new();
    sink.write(&sample_entry("a")).expect("audit operation");
    sink.write(&sample_entry("b")).expect("audit operation");
    assert_eq!(sink.len(), 2);
    assert_eq!(sink.entries()[0].tool_name, "mcp::fetch::fetch");
    sink.clear();
    assert!(sink.is_empty());
}

#[test]
fn null_sink_accepts_without_recording() {
    let sink = NullSink;
    sink.write(&sample_entry("x")).expect("audit operation");
    // No observable state, but the call must not panic.
}

#[test]
fn jsonl_file_sink_appends_and_flushes_on_drop() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    let sink = JsonlFileSink::open(&path, 1024 * 1024, 4).expect("open sink");

    sink.write(&sample_entry("a")).expect("audit operation");
    sink.write(&sample_entry("b")).expect("audit operation");
    sink.flush().expect("audit operation");

    let body = std::fs::read_to_string(&path).expect("read back");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let value: Value = serde_json::from_str(line).expect("line is valid JSON");
        assert_eq!(value["tool_name"], "mcp::fetch::fetch");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn jsonl_file_sink_io_failure_logs_safe_fields_without_audit_path() {
    const SENTINEL_PATH: &str = "CUSTOM_AUDIT_PATH_7bd907a9";

    let file = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("Linux /dev/full should be available for the failing writer");
    let sink = JsonlFileSink {
        path: PathBuf::from(format!("/private/audit/{SENTINEL_PATH}/audit.jsonl")),
        max_size_bytes: u64::MAX,
        max_files: 1,
        state: Mutex::new(JsonlFileState {
            writer: Some(BufWriter::with_capacity(0, file)),
            bytes_written: 0,
        }),
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing::Dispatch::new(CapturedEvents {
        events: Arc::clone(&events),
        next_span_id: AtomicU64::default(),
    });

    let error = tracing::dispatcher::with_default(&subscriber, || sink.write(&sample_entry("failure")))
        .expect_err("full device must propagate a write failure");
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);

    let events = events.lock().expect("captured event lock should be available");
    let logs = events.join("\n");
    assert!(!logs.contains(SENTINEL_PATH), "audit path leaked into logs: {logs}");
    assert!(logs.contains("operation=\"write\""), "operation should remain observable: {logs}");
    assert!(logs.contains("error_kind=StorageFull"), "bounded I/O kind should remain observable: {logs}");
    assert!(logs.contains("outcome=\"failed\""), "failure outcome should remain observable: {logs}");
}

#[cfg(target_os = "linux")]
#[test]
fn jsonl_file_sink_propagates_buffer_flush_and_durable_sync_failures() {
    for (device, buffered, expected_kind) in [
        ("/dev/full", true, std::io::ErrorKind::StorageFull),
        ("/dev/null", false, std::io::ErrorKind::InvalidInput),
    ] {
        let file = std::fs::OpenOptions::new().write(true).open(device).expect("Linux device");
        let sink = JsonlFileSink {
            path: PathBuf::from("unused-audit-path"),
            max_size_bytes: u64::MAX,
            max_files: 1,
            state: Mutex::new(JsonlFileState {
                writer: Some(BufWriter::new(file)),
                bytes_written: 0,
            }),
        };
        if buffered {
            sink.write(&sample_entry("buffered")).expect("entry stays buffered");
        }
        let logger = ToolAuditLogger::new(Arc::new(sink));
        assert_eq!(logger.flush().expect_err("durability failure must propagate").kind(), expected_kind);
    }
}

#[test]
fn jsonl_file_sink_rotates_when_threshold_exceeded() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    // Tiny rotation threshold so the second entry triggers a rotate.
    let sink = JsonlFileSink::open(&path, 60, 3).expect("open sink");

    sink.write(&sample_entry("a")).expect("audit operation");
    sink.flush().expect("audit operation");
    sink.write(&sample_entry("b")).expect("audit operation");
    sink.flush().expect("audit operation");

    // After rotation, the original path should contain the most recent entry
    // and `<path>.1` should contain the older one.
    let active = std::fs::read_to_string(&path).expect("active");
    assert!(active.contains("\"call-b\""), "expected rotated active file to contain call-b, got: {active}");
    let rotated = std::fs::read_to_string(dir.path().join("audit.jsonl.1")).expect("rotated");
    assert!(rotated.contains("\"call-a\""), "expected rotated file to contain call-a, got: {rotated}");
}

#[cfg(unix)]
#[test]
fn jsonl_file_sink_recovers_after_rotation_rename_failure() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    let rotated_path = dir.path().join("audit.jsonl.1");
    let sink = JsonlFileSink::open(&path, 1, 1).expect("open sink");

    sink.write(&sample_entry("before-failure")).expect("initial audit entry");
    std::fs::create_dir(&rotated_path).expect("block rotation destination");
    let error = sink
        .write(&sample_entry("rejected-during-failure"))
        .expect_err("rotation failure must be returned to the caller");
    assert_eq!(error.kind(), std::io::ErrorKind::IsADirectory);
    sink.flush().expect("writer remains usable after failed rotation");

    std::fs::remove_dir(&rotated_path).expect("remove rotation blocker");
    sink.write(&sample_entry("after-recovery")).expect("rotation and write recover");
    sink.flush().expect("recovered entry is durable");

    let active = std::fs::read_to_string(&path).expect("active audit file");
    let rotated = std::fs::read_to_string(&rotated_path).expect("rotated audit file");
    assert!(active.contains("after-recovery"), "recovered entry should reach active file: {active}");
    assert!(rotated.contains("before-failure"), "original entry should remain rotated: {rotated}");
}

#[test]
fn multi_sink_forwards_to_every_inner_sink() {
    let a = Arc::new(InMemorySink::new());
    let b = Arc::new(InMemorySink::new());
    let multi = MultiSink::new(vec![a.clone(), b.clone()]);
    multi.write(&sample_entry("z")).expect("audit operation");
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
}

#[test]
fn tool_audit_logger_record_routes_through_sink() {
    let sink = Arc::new(InMemorySink::new());
    let logger = ToolAuditLogger::new(sink.clone());
    logger.record(sample_entry("k")).expect("audit operation");
    logger.flush().expect("audit operation");
    assert_eq!(sink.len(), 1);
}

#[test]
fn logger_propagates_write_and_flush_failures_and_fanout_continues() {
    struct FailingSink;
    impl ToolAuditSink for FailingSink {
        fn write(&self, _entry: &ToolAuditEntry) -> std::io::Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "write failed"))
        }
        fn flush(&self) -> std::io::Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "sync failed"))
        }
    }
    let recorded = Arc::new(InMemorySink::new());
    let logger = ToolAuditLogger::new(Arc::new(MultiSink::new(vec![Arc::new(FailingSink), recorded.clone()])));
    assert_eq!(
        logger.record(sample_entry("failure")).expect_err("write failure").kind(),
        std::io::ErrorKind::StorageFull
    );
    assert_eq!(recorded.len(), 1, "a failed sink must not suppress other sinks");
    assert_eq!(logger.flush().expect_err("flush failure").kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn sha256_hex_is_stable() {
    assert_eq!(sha256_hex(b"hello"), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    assert_eq!(sha256_hex(b"hello"), sha256_hex(b"hello"));
}
