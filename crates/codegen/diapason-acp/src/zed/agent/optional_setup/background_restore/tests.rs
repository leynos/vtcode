//! Regressions for owner migration and launch rollback.
use super::*;
use assert_fs::TempDir;
use serde_json::json;

async fn write_ledger(workspace: &Path, records: serde_json::Value) {
    let path = ledger_path(workspace);
    fs::create_dir_all(path.parent().expect("ledger parent"))
        .await
        .expect("state directory");
    fs::write(&path, serde_json::to_string_pretty(&json!({ "records": records })).expect("serialize ledger"))
        .await
        .expect("write ledger");
}

async fn read_ledger(workspace: &Path) -> serde_json::Value {
    let raw = fs::read_to_string(ledger_path(workspace)).await.expect("read ledger");
    serde_json::from_str(&raw).expect("parse ledger")
}

fn entry(id: &str, exec_session_id: &str) -> BackgroundSubprocessEntry {
    entry_with_status(id, exec_session_id, "running")
}

/// Builds a status fixture, so a failed spawn that keeps its newly assigned
/// exec session can be represented as the `error` record core settles.
fn entry_with_status(id: &str, exec_session_id: &str, status: &str) -> BackgroundSubprocessEntry {
    serde_json::from_value(json!({
        "id": id,
        "session_id": format!("session-{id}"),
        "exec_session_id": exec_session_id,
        "agent_name": "watcher",
        "display_label": id,
        "description": id,
        "source": "test",
        "status": status,
        "desired_enabled": true,
        "created_at": "2026-08-14T12:00:00Z",
        "updated_at": "2026-08-14T12:00:00Z",
    }))
    .expect("background entry fixture")
}

/// A row this session launched must become owned by it, so the workspace
/// restore skips it and the session controller adopts it. A row another
/// session launched, and one that already has an owner, must be untouched.
#[tokio::test]
async fn a_legacy_row_is_adopted_by_the_session_that_launched_it() {
    let temp = TempDir::new().expect("temporary workspace");
    write_ledger(
        temp.path(),
        json!([
            {
                "id": "background-watcher",
                "agent_name": "watcher",
                "parent_session_id": "session-a",
                "exec_session_id": "exec-a",
                "desired_enabled": true,
            },
            {
                "id": "background-watcher@other",
                "agent_name": "watcher",
                "owner_session_id": "other",
                "parent_session_id": "session-a",
                "exec_session_id": "exec-b",
                "desired_enabled": true,
            },
            {
                "id": "background-watcher-foreign",
                "agent_name": "watcher",
                "parent_session_id": "session-b",
                "exec_session_id": "exec-c",
                "desired_enabled": true,
            },
        ]),
    )
    .await;

    let migrated = migrate_legacy_owner_rows(temp.path(), "session-a").await.expect("migration");

    assert_eq!(migrated, 1, "only the unowned row launched by this session is migrated");
    let ledger = read_ledger(temp.path()).await;
    let records = ledger["records"].as_array().expect("records");
    assert_eq!(records[0]["owner_session_id"], "session-a");
    assert_eq!(records[0]["parent_session_id"], "session-a", "ownership must not rewrite the launcher");
    assert_eq!(records[0]["exec_session_id"], "exec-a", "the launch identity must be preserved");
    assert_eq!(records[0]["desired_enabled"], true);
    assert_eq!(records[1]["owner_session_id"], "other", "an owned row must never change hands");
    assert_eq!(records[2].get("owner_session_id"), None, "another session's row stays ownerless");
}

/// A retried attach must not reinterpret an already migrated row.
#[tokio::test]
async fn migration_is_idempotent() {
    let temp = TempDir::new().expect("temporary workspace");
    write_ledger(
        temp.path(),
        json!([{
            "id": "background-watcher",
            "agent_name": "watcher",
            "parent_session_id": "session-a",
            "exec_session_id": "exec-a",
            "desired_enabled": true,
        }]),
    )
    .await;

    assert_eq!(
        migrate_legacy_owner_rows(temp.path(), "session-a")
            .await
            .expect("first migration"),
        1
    );
    assert_eq!(
        migrate_legacy_owner_rows(temp.path(), "session-a")
            .await
            .expect("second migration"),
        0
    );
}

/// A workspace with no ledger has nothing to migrate, and the migration
/// must not create one.
#[tokio::test]
async fn a_missing_ledger_is_a_no_op() {
    let temp = TempDir::new().expect("temporary workspace");
    assert_eq!(migrate_legacy_owner_rows(temp.path(), "session-a").await.expect("migration"), 0);
    assert!(!ledger_path(temp.path()).exists(), "a no-op migration must not create the ledger");
}

/// Only a changed, running, unrecorded launch may be rolled back. An
/// unchanged record predates the pass, a recorded launch is recoverable, a
/// record whose exec session was cleared names no process, and a record a
/// failed spawn settled `Error` never had a process to stop.
#[test]
fn rollback_selects_only_launches_the_ledger_does_not_record() {
    let before = HashMap::from([
        ("background-a".to_string(), "exec-old-a".to_string()),
        ("background-b".to_string(), "exec-old-b".to_string()),
        ("background-c".to_string(), "exec-old-c".to_string()),
        ("background-d".to_string(), "exec-old-d".to_string()),
        ("background-e".to_string(), "exec-old-e".to_string()),
    ]);
    let after = vec![
        entry("background-a", "exec-new-a"),
        entry("background-b", "exec-old-b"),
        entry("background-c", ""),
        entry("background-d", "exec-new-d"),
        entry_with_status("background-e", "exec-new-e", "error"),
    ];
    let persisted = PersistedLaunches::Known(HashSet::from(["exec-new-d".to_string()]));

    assert_eq!(
        unpersisted_launches(&before, &after, &persisted),
        vec!["background-a".to_string()],
        "only the running launch that changed and was not persisted may be stopped"
    );
}

/// An unverifiable ledger must not authorise stopping anything: a launch it
/// might still record could be durable, and the in-memory controller still
/// tracks the live process.
#[test]
fn rollback_selects_nothing_when_the_ledger_is_unverifiable() {
    let before = HashMap::from([("background-a".to_string(), "exec-old-a".to_string())]);
    let after = vec![entry("background-a", "exec-new-a")];

    assert!(
        unpersisted_launches(&before, &after, &PersistedLaunches::Unknown).is_empty(),
        "an unverifiable ledger must leave a possibly durable launch running"
    );
}

/// A missing ledger still means no launch is recorded, so the launch this
/// pass made is rolled back.
#[test]
fn rollback_selects_a_launch_when_no_ledger_exists() {
    let before = HashMap::from([("background-a".to_string(), "exec-old-a".to_string())]);
    let after = vec![entry("background-a", "exec-new-a")];

    assert_eq!(
        unpersisted_launches(&before, &after, &PersistedLaunches::Absent),
        vec!["background-a".to_string()],
        "an absent ledger records no launch"
    );
}

/// The ledger is the only record of which launches survived a failed save.
#[tokio::test]
async fn persisted_exec_sessions_are_read_from_the_ledger() {
    let temp = TempDir::new().expect("temporary workspace");
    write_ledger(
        temp.path(),
        json!([
            { "id": "background-a", "exec_session_id": "exec-a" },
            { "id": "background-b", "exec_session_id": "" },
            { "id": "background-c" },
        ]),
    )
    .await;

    assert_eq!(
        persisted_launches(temp.path()).await,
        PersistedLaunches::Known(HashSet::from(["exec-a".to_string()])),
        "empty and absent exec sessions are not persisted launches"
    );
}

/// A workspace with no ledger records no launch.
#[tokio::test]
async fn a_missing_ledger_records_no_persisted_launch() {
    let temp = TempDir::new().expect("temporary workspace");
    assert_eq!(persisted_launches(temp.path()).await, PersistedLaunches::Absent);
}

/// A ledger that cannot be read is unverifiable, not empty: the launch may
/// still be durable, so rollback must not treat it as unrecorded.
#[tokio::test]
async fn a_faulting_ledger_makes_launches_unverifiable() {
    let temp = TempDir::new().expect("temporary workspace");
    let path = ledger_path(temp.path());
    fs::create_dir_all(path.parent().expect("ledger parent"))
        .await
        .expect("state directory");
    fs::create_dir(&path).await.expect("occupy the ledger path with a directory");

    assert_eq!(
        persisted_launches(temp.path()).await,
        PersistedLaunches::Unknown,
        "an unreadable ledger must not be degraded to an empty set"
    );
}

/// A ledger whose contents cannot be parsed is likewise unverifiable.
#[tokio::test]
async fn a_malformed_ledger_makes_launches_unverifiable() {
    let temp = TempDir::new().expect("temporary workspace");
    let path = ledger_path(temp.path());
    fs::create_dir_all(path.parent().expect("ledger parent"))
        .await
        .expect("state directory");
    fs::write(&path, b"{ not json").await.expect("write malformed ledger");

    assert_eq!(
        persisted_launches(temp.path()).await,
        PersistedLaunches::Unknown,
        "a parse failure must not be degraded to an empty set"
    );
}
