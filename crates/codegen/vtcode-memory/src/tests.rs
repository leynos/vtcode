//! Tests for the unified session store.

use std::fs;

use tempfile::TempDir;
use vtcode_exec_events::{
    ThreadCompletedEvent, ThreadCompletionSubtype, ThreadEvent, ThreadStartedEvent, TurnCompletedEvent,
    TurnStartedEvent, Usage, VersionedThreadEvent,
};

use crate::event_log::{DEFAULT_MAX_EVENTS, SessionEventLog};
use crate::migration::migrate_legacy;
use crate::query::{query_facts, recent_sessions};
use crate::{open, retention::apply_retention, sessions_root};

fn sample_turn() -> Vec<ThreadEvent> {
    vec![
        ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }),
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ThreadEvent::ThreadCompleted(ThreadCompletedEvent {
            thread_id: "thread".to_string(),
            session_id: "session".to_string(),
            subtype: ThreadCompletionSubtype::Success,
            outcome_code: "completed".to_string(),
            result: None,
            stop_reason: None,
            usage: Usage::default(),
            total_cost_usd: None,
            num_turns: 1,
        }),
    ]
}

#[test]
fn append_and_reconstruct_roundtrip() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-1", DEFAULT_MAX_EVENTS).expect("open");
    for _ in 0..3 {
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
    }
    assert_eq!(log.turn_count(), 3);
    let rebuilt = log.reconstruct_turn(2).expect("reconstruct");
    assert_eq!(rebuilt.len(), 2);
    assert!(matches!(rebuilt[0], ThreadEvent::TurnStarted(_)));
    assert!(matches!(rebuilt[1], ThreadEvent::TurnCompleted(_)));
}

#[test]
fn event_log_batches_appends_until_turn_boundary() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-buffered", DEFAULT_MAX_EVENTS).expect("open");
    let events_path = sessions_root(dir.path()).join("sess-buffered").join("events.jsonl");

    log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }))
        .expect("append thread event");
    assert_eq!(fs::metadata(&events_path).expect("metadata").len(), 0);

    log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
        .expect("append turn start");
    assert_eq!(fs::metadata(&events_path).expect("metadata").len(), 0);

    log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
        .expect("append turn completion");
    assert!(fs::metadata(&events_path).expect("metadata").len() > 0);
    assert_eq!(log.reconstruct_turn(1).expect("reconstruct").len(), 2);
}

#[test]
fn large_buffer_flush_persists_manifest_progress() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-large-buffer", DEFAULT_MAX_EVENTS).expect("open");

    for index in 0..1_000 {
        log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent {
            thread_id: format!("thread-{index:04}-buffer-boundary"),
        }))
        .expect("append event");
    }

    let manifest_path = sessions_root(dir.path()).join("sess-large-buffer").join("manifest.json");
    let manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(manifest_path).expect("read manifest")).expect("parse manifest");
    assert!(manifest.event_count > 0);
    assert!(manifest.event_count < 1_000);
}

#[test]
fn flushing_mid_turn_persists_buffered_metadata_for_reopen() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-mid-turn", DEFAULT_MAX_EVENTS).expect("open");
        log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }))
            .expect("append thread event");
        log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
            .expect("append turn start");
        log.flush().expect("flush mid-turn event log");
    }

    let reopened = open(dir.path(), "sess-mid-turn", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(reopened.event_count(), 2);
    assert_eq!(reopened.turn_count(), 0);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct open turn").len(), 1);
}

#[test]
fn index_rebuilt_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-2", DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
    }
    // Reopen: scan must rebuild the index from events.jsonl.
    let log = SessionEventLog::open(dir.path(), "sess-2", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(log.turn_count(), 1);
    let rebuilt = log.reconstruct_turn(1).expect("reconstruct after reopen");
    assert_eq!(rebuilt.len(), 2);
    assert!(log.manifest().status == "completed");
}

#[test]
fn migrate_legacy_imports_history_and_trajectory() {
    let dir = TempDir::new().expect("tempdir");
    let vt = dir.path().join(".vtcode");
    fs::create_dir_all(vt.join("history")).expect("mk history");
    fs::create_dir_all(vt.join("logs")).expect("mk logs");

    let memory = serde_json::json!({
        "session_id": "session-foo",
        "schema_version": 2,
        "summary": "did a thing",
        "grounded_facts": [{"fact": "the widget is blue"}],
    });
    fs::write(
        vt.join("history").join("session-foo.memory.json"),
        serde_json::to_string_pretty(&memory).expect("ser"),
    )
    .expect("write memory");

    fs::write(
        vt.join("logs").join("trajectory-20260101T000000Z.jsonl"),
        "{\"kind\":\"llm_retry_metrics\",\"turn\":1}\n",
    )
    .expect("write traj");

    let report = migrate_legacy(dir.path(), false).expect("migrate");
    assert_eq!(report.sessions_created, 2);
    assert_eq!(report.memory_imported, 1);
    assert_eq!(report.trajectory_imported, 1);

    // Cross-session fact query works off the unified store.
    let facts = query_facts(dir.path(), 10).expect("facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].fact, "the widget is blue");

    // Legacy history + logs still present (remove_legacy=false).
    assert!(vt.join("history").exists());
    assert!(vt.join("logs").exists());

    // recent_sessions lists the migrated sessions.
    let sessions = recent_sessions(dir.path(), 10);
    assert_eq!(sessions.len(), 2);
}

#[test]
fn retention_removes_oldest_sessions() {
    let dir = TempDir::new().expect("tempdir");
    // Create 3 old sessions (2020) and 2 recent sessions (today).
    for i in 0..5u64 {
        let log = open(dir.path(), &format!("sess-{i}"), DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
        let mpath = sessions_root(dir.path()).join(format!("sess-{i}")).join("manifest.json");
        let mut m: crate::SessionManifest =
            serde_json::from_str(&fs::read_to_string(&mpath).expect("read manifest")).expect("parse");
        // First 3 are old (2020), last 2 keep today's timestamp.
        if i < 3 {
            m.updated_at = format!("2020-01-{:02}T00:00:00Z", i + 1);
            fs::write(&mpath, serde_json::to_string_pretty(&m).expect("ser")).expect("write manifest");
        }
    }

    // max_sessions=4: count-based eviction removes 1 oldest (sess-0).
    // max_age_days=30: age-based eviction removes 2 more old sessions (sess-1, sess-2).
    // Total: 3 removed, 2 recent remain.
    let removed = apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 4, max_age_days: 30 })
        .expect("retain");
    assert_eq!(removed, 3);
    let remaining = recent_sessions(dir.path(), 100);
    assert_eq!(remaining.len(), 2);
}

#[test]
fn retention_evicts_old_sessions_even_when_under_count_cap() {
    let dir = TempDir::new().expect("tempdir");
    // Create 3 sessions: 1 old (2020) and 2 recent (today).
    for i in 0..3u64 {
        let log = open(dir.path(), &format!("sess-{i}"), DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
        if i == 0 {
            let mpath = sessions_root(dir.path()).join("sess-0").join("manifest.json");
            let mut m: crate::SessionManifest =
                serde_json::from_str(&fs::read_to_string(&mpath).expect("read manifest")).expect("parse");
            m.updated_at = "2020-01-01T00:00:00Z".to_string();
            fs::write(&mpath, serde_json::to_string_pretty(&m).expect("ser")).expect("write manifest");
        }
    }

    // max_sessions=10: count cap is not exceeded (3 < 10).
    // max_age_days=30: age-based eviction should still remove sess-0.
    let removed = apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 10, max_age_days: 30 })
        .expect("retain");
    assert_eq!(removed, 1);
    let remaining = recent_sessions(dir.path(), 100);
    assert_eq!(remaining.len(), 2);
}

#[test]
fn retention_preserves_active_sessions() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "active-session", DEFAULT_MAX_EVENTS).expect("open");
    log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "active".to_string() }))
        .expect("append thread start");
    log.flush().expect("flush active session");

    let session_dir = sessions_root(dir.path()).join("active-session");
    let manifest_path = session_dir.join("manifest.json");
    let mut manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).expect("serialize manifest"))
        .expect("write manifest");

    let removed = apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 0 })
        .expect("retain");

    assert_eq!(removed, 0);
    assert!(session_dir.exists(), "active sessions must not be evicted");
}

#[test]
fn retention_preserves_explicit_current_session() {
    let dir = TempDir::new().expect("tempdir");
    for session_id in ["current-session", "old-session"] {
        let log = open(dir.path(), session_id, DEFAULT_MAX_EVENTS).expect("open");
        for event in &sample_turn() {
            log.append(event).expect("append lifecycle");
        }
        log.complete().expect("complete");
        let manifest_path = sessions_root(dir.path()).join(session_id).join("manifest.json");
        let mut manifest: crate::SessionManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
        manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
        fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).expect("serialize manifest"))
            .expect("write manifest");
    }

    let removed = crate::retention::apply_retention_preserving(
        dir.path(),
        crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 0 },
        Some("current-session"),
    )
    .expect("retain");

    assert_eq!(removed, 1);
    assert!(sessions_root(dir.path()).join("current-session").exists());
    assert!(!sessions_root(dir.path()).join("old-session").exists());
}

#[test]
fn retention_ignores_manifest_session_id_for_deletion_path() {
    let dir = TempDir::new().expect("tempdir");
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).expect("create outside");
    fs::write(outside.join("keep.txt"), "preserve").expect("write outside file");

    let log = open(dir.path(), "safe-session", DEFAULT_MAX_EVENTS).expect("open");
    for event in &sample_turn() {
        log.append(event).expect("append lifecycle");
    }
    log.complete().expect("complete");
    let manifest_path = sessions_root(dir.path()).join("safe-session").join("manifest.json");
    let mut manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.session_id = "../outside".to_string();
    manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
    fs::write(&manifest_path, serde_json::to_string(&manifest).expect("serialize manifest")).expect("write manifest");

    assert_eq!(
        apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 30 })
            .expect("retain"),
        1
    );
    assert!(outside.join("keep.txt").exists());
}

#[cfg(unix)]
#[test]
fn retention_skips_symlink_session_entries() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("tempdir");
    let target = sessions_root(dir.path()).join("target");
    let log = open(dir.path(), "target", DEFAULT_MAX_EVENTS).expect("open");
    for event in &sample_turn() {
        log.append(event).expect("append lifecycle");
    }
    log.complete().expect("complete");
    let manifest_path = target.join("manifest.json");
    let mut manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
    fs::write(&manifest_path, serde_json::to_string(&manifest).expect("serialize manifest")).expect("write manifest");
    let link = sessions_root(dir.path()).join("linked");
    symlink(&target, &link).expect("create symlink");

    apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 30 })
        .expect("retain");
    assert!(!target.exists());
    assert!(link.exists() || fs::symlink_metadata(&link).is_ok());
}

#[cfg(unix)]
#[test]
fn retention_skips_symlink_sessions_root() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("tempdir");
    let real_root = dir.path().join("real-sessions");
    let linked_session = real_root.join("linked-session");
    fs::create_dir_all(&linked_session).expect("create linked session");
    fs::write(
        linked_session.join("manifest.json"),
        serde_json::json!({
            "session_id": "linked-session",
            "turn_count": 0,
            "event_count": 0,
            "status": "completed",
            "updated_at": "2020-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("write linked manifest");

    let sessions_parent = dir.path().join(".vtcode");
    fs::create_dir_all(&sessions_parent).expect("create sessions parent");
    symlink(&real_root, sessions_parent.join("sessions")).expect("create sessions root symlink");

    assert_eq!(
        apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 0 })
            .expect("retain"),
        0
    );
    assert!(linked_session.exists());
}

#[test]
fn manifest_shortcut_skips_scan_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    // Write a few turns and complete.
    {
        let log = open(dir.path(), "sess-shortcut", DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
    }
    // Reopen: the manifest + index should be loaded without scanning.
    let log = SessionEventLog::open(dir.path(), "sess-shortcut", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(log.turn_count(), 1);
    assert_eq!(log.manifest().status, "completed");
    let rebuilt = log.reconstruct_turn(1).expect("reconstruct");
    assert_eq!(rebuilt.len(), 2);
}

#[test]
fn corrupt_manifest_falls_back_to_event_log_scan() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-corrupt-manifest", DEFAULT_MAX_EVENTS).expect("open");
        for event in &sample_turn() {
            log.append(event).expect("append");
        }
        log.complete().expect("complete");
    }

    let session_dir = sessions_root(dir.path()).join("sess-corrupt-manifest");
    fs::write(session_dir.join("manifest.json"), b"{\"broken\"").expect("corrupt manifest");

    let reopened = open(dir.path(), "sess-corrupt-manifest", DEFAULT_MAX_EVENTS).expect("recover from manifest");
    assert_eq!(reopened.turn_count(), 1);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct").len(), 2);
    serde_json::from_str::<crate::SessionManifest>(
        &fs::read_to_string(session_dir.join("manifest.json")).expect("read repaired manifest"),
    )
    .expect("manifest should be repaired");
}

#[test]
fn stale_turn_index_offsets_fall_back_to_event_log_scan() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-stale-index", DEFAULT_MAX_EVENTS).expect("open");
        for event in &sample_turn() {
            log.append(event).expect("append");
        }
        log.complete().expect("complete");
    }

    let index_path = sessions_root(dir.path()).join("sess-stale-index/index/turns.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&index_path).expect("read index")).expect("parse index");
    index["entries"][0]["end_offset"] = serde_json::json!(u64::MAX);
    fs::write(&index_path, serde_json::to_vec(&index).expect("serialize stale index")).expect("write stale index");

    let reopened = open(dir.path(), "sess-stale-index", DEFAULT_MAX_EVENTS).expect("recover from stale index");
    assert_eq!(reopened.turn_count(), 1);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct").len(), 2);
}

#[test]
fn cap_rewrite_keeps_event_log_appendable_and_reopenable() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-cap-rewrite", 2).expect("open");
    let turn = || {
        [
            ThreadEvent::TurnStarted(TurnStartedEvent::default()),
            ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ]
    };

    for event in turn().into_iter().chain(turn()) {
        log.append(&event).expect("append");
    }

    let events_path = sessions_root(dir.path()).join("sess-cap-rewrite/events.jsonl");
    assert_eq!(fs::read_to_string(&events_path).expect("read compacted log").lines().count(), 2);
    assert_eq!(log.reconstruct_turn(2).expect("reconstruct retained turn").len(), 2);

    drop(log);
    let reopened = open(dir.path(), "sess-cap-rewrite", 2).expect("reopen compacted log");
    assert_eq!(reopened.event_count(), 2);
    assert_eq!(reopened.reconstruct_turn(2).expect("reconstruct after reopen").len(), 2);
}

#[cfg(unix)]
#[test]
fn session_artefacts_use_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-private", DEFAULT_MAX_EVENTS).expect("open");
    for event in &sample_turn() {
        log.append(event).expect("append");
    }
    log.flush().expect("flush");

    let session_dir = sessions_root(dir.path()).join("sess-private");
    let mode = |path: &std::path::Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&session_dir), 0o700);
    assert_eq!(mode(&session_dir.join("derived")), 0o700);
    assert_eq!(mode(&session_dir.join("index")), 0o700);
    assert_eq!(mode(&session_dir.join("events.jsonl")), 0o600);
    assert_eq!(mode(&session_dir.join("manifest.json")), 0o600);
    assert_eq!(mode(&session_dir.join("index/turns.json")), 0o600);
}

#[test]
fn scan_fallback_when_manifest_missing() {
    let dir = TempDir::new().expect("tempdir");
    // Write events directly to events.jsonl without manifest/index.
    let session_dir = dir.path().join(".vtcode/sessions/sess-raw");
    let events_path = session_dir.join("events.jsonl");
    fs::create_dir_all(&session_dir).expect("mkdir");
    let events = [
        VersionedThreadEvent::new(ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "t-1".to_string() })),
        VersionedThreadEvent::new(ThreadEvent::TurnStarted(TurnStartedEvent::default())),
        VersionedThreadEvent::new(ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() })),
    ];
    let lines: Vec<String> = events.iter().map(|v| serde_json::to_string(v).expect("ser")).collect();
    fs::write(&events_path, lines.join("\n") + "\n").expect("write raw events");

    let log = SessionEventLog::open(dir.path(), "sess-raw", DEFAULT_MAX_EVENTS).expect("open");
    assert_eq!(log.turn_count(), 1);
    let rebuilt = log.reconstruct_turn(1).expect("reconstruct");
    assert_eq!(rebuilt.len(), 2);
}

#[test]
fn scan_skips_malformed_lifecycle_payloads() {
    let dir = TempDir::new().expect("tempdir");
    let session_dir = dir.path().join(".vtcode/sessions/sess-invalid");
    let events_path = session_dir.join("events.jsonl");
    fs::create_dir_all(&session_dir).expect("mkdir");

    let valid_events = [
        VersionedThreadEvent::new(ThreadEvent::TurnStarted(TurnStartedEvent::default())),
        VersionedThreadEvent::new(ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() })),
    ];
    let mut lines = vec![
        r#"{"schema_version":"0.11.0","event":{"type":"thread.started","thread_id":123}}"#.to_string(),
        r#"{"schema_version":"0.11.0","event":{"type":"turn.started","token_breakdown":"invalid"}}"#.to_string(),
        r#"{"schema_version":"0.11.0","event":{"type":"turn.completed","usage":{}}}"#.to_string(),
    ];
    lines.extend(
        valid_events
            .iter()
            .map(|event| serde_json::to_string(event).expect("serialize")),
    );
    fs::write(&events_path, lines.join("\n") + "\n").expect("write raw events");

    let log = SessionEventLog::open(dir.path(), "sess-invalid", DEFAULT_MAX_EVENTS).expect("open");
    assert_eq!(log.event_count(), 2);
    assert_eq!(log.turn_count(), 1);
    assert_eq!(log.reconstruct_turn(1).expect("reconstruct").len(), 2);
}
