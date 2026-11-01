//! Tool-call repair when an archived ACP session is resumed.
//!
//! Covers placeholder insertion for legacy archives, relocation of a uniquely
//! attributable late result, refusal of ambiguous history, and the durable
//! checkpoint that must precede registration.
use super::super::archive::repair_archived_tool_calls;
use super::archive::archived_listing;
use super::*;
use crate::zed::session::AcpSessionWorkspace;
use assert_fs::TempDir;
use vtcode_core::core::threads::messages_from_session_listing;
use vtcode_core::llm::provider::{MessageRole, ToolCall};

#[test]
fn legacy_archive_repair_closes_tool_call_before_later_user_message() {
    let temp = TempDir::new().unwrap();
    let mut listing = archived_listing("legacy-unresolved", temp.path(), "continue");
    let unresolved = Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "legacy-call".to_string(),
            "apply_patch".to_string(),
            "{}".to_string(),
        )],
    );
    listing.snapshot.messages = vec![
        SessionMessage::from(&unresolved),
        SessionMessage::from(&Message::user("continue".to_string())),
    ];

    let report = repair_archived_tool_calls(&mut listing).expect("unambiguous archive should repair");
    let repaired = messages_from_session_listing(&listing);

    assert_eq!(report.repaired_calls, 1);
    assert_eq!(repaired.len(), 3);
    assert_eq!(repaired[0].role, MessageRole::Assistant);
    assert_eq!(repaired[1].role, MessageRole::Tool);
    assert_eq!(repaired[1].tool_call_id.as_deref(), Some("legacy-call"));
    assert!(repaired[1].content.as_text().contains("\"replayed\":false"));
    assert_eq!(repaired[2].role, MessageRole::User);
}

#[test]
fn relocation_only_repair_is_persisted_to_the_archive() {
    let temp = TempDir::new().unwrap();
    let mut listing = archived_listing("relocation-only", temp.path(), "continue");
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "late-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::user("continue".to_string())),
        SessionMessage::from(&Message::tool_response("late-call".to_string(), "late result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();

    let report = repair_archived_tool_calls(&mut listing).expect("relocation-only repair should succeed");
    let repaired = messages_from_session_listing(&listing);

    assert_eq!(report.repaired_calls, 0);
    assert_eq!(report.relocated_calls, 1);
    assert_eq!(repaired.len(), 3);
    assert_eq!(repaired[0].role, MessageRole::Assistant);
    assert_eq!(repaired[1].role, MessageRole::Tool);
    assert_eq!(repaired[1].tool_call_id.as_deref(), Some("late-call"));
    assert_eq!(repaired[1].content.as_text(), "late result");
    assert_eq!(repaired[2].role, MessageRole::User);
}

#[test]
fn truncated_archive_prefix_resumes_without_ambiguity() {
    // Front compaction can retain tool results whose assistant request was
    // trimmed away; that shape must resume rather than being rejected.
    let temp = TempDir::new().unwrap();
    let mut listing = archived_listing("truncated-prefix", temp.path(), "continue");
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::tool_response("trimmed-call".to_string(), "earlier result".to_string())),
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "live-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response("live-call".to_string(), "live result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();

    let report = repair_archived_tool_calls(&mut listing).expect("front-truncated archives must stay resumable");

    assert!(report.truncated_prefix >= 1);
    assert_eq!(report.repaired_calls, 0);
}

#[tokio::test]
async fn attaching_legacy_archive_repairs_without_replaying_mutating_call() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("must-not-change.txt");
    fs::write(&target, "original\n").unwrap();
    let patch =
        format!("*** Begin Patch\n*** Update File: {}\n@@\n-original\n+replayed\n*** End Patch\n", target.display());
    let unresolved = Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "legacy-apply-patch".to_string(),
            "apply_patch".to_string(),
            serde_json::json!({"patch": patch}).to_string(),
        )],
    );
    let mut listing = archived_listing("legacy-mutating", temp.path(), "edit the file");
    listing.snapshot.messages = vec![SessionMessage::from(&unresolved)];
    listing.snapshot.total_messages = 1;
    let archive_path = listing.path.clone();
    let agent = build_agent(temp.path()).await;

    let handle = agent
        .attach_thread_from_listing(
            &acp::SessionId::new("loaded-legacy-mutating"),
            &AcpSessionWorkspace::from_client_cwd(temp.path()).await.unwrap(),
            listing,
        )
        .await
        .unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
    let messages = handle.data.lock().unwrap().thread.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert_eq!(messages[1].role, MessageRole::Tool);
    let recovery = messages[1].content.as_text();
    assert!(recovery.contains("\"replayed\":false"));
    assert!(recovery.contains("Verify the current workspace state"));
    assert!(recovery.contains("resubmit the tool call only if"));

    let persisted: SessionSnapshot = serde_json::from_str(&fs::read_to_string(archive_path).unwrap()).unwrap();
    assert_eq!(persisted.messages.len(), 2);
    assert_eq!(persisted.messages[1].role, MessageRole::Tool);
}

#[tokio::test]
async fn repaired_archive_is_not_registered_when_repair_checkpoint_fails() {
    // Reviews 3791762677 and 3944816962: a resumed session is visible to
    // callers only after its repaired history has been durably written.
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let mut listing = archived_listing("repair-checkpoint-failure", temp.path(), "continue");
    let unresolved = Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "repair-call".to_string(),
            "apply_patch".to_string(),
            "{}".to_string(),
        )],
    );
    listing.snapshot.messages = vec![SessionMessage::from(&unresolved)];
    listing.snapshot.total_messages = 1;
    fs::create_dir(&listing.path).unwrap();

    let session_id = acp::SessionId::new("repair-checkpoint-failure-session");
    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path()).await.unwrap(),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a directory at the archive path must make the repair checkpoint fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("Failed to persist repaired ACP tool-call history"));
    assert!(
        agent.session_handle(&session_id).unwrap().is_none(),
        "failed repair checkpoints must not leave a partially registered session"
    );
}

#[tokio::test]
async fn ambiguous_archive_is_not_resumed_or_replayed_and_preserves_the_archive() {
    // A duplicate result has a known external-effect record, but no safe
    // canonical provider continuation. Reject before the archive can be
    // rewritten or the session becomes eligible for a prompt/tool call.
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("must-not-change.txt");
    fs::write(&target, "original\n").unwrap();
    let patch =
        format!("*** Begin Patch\n*** Update File: {}\n@@\n-original\n+replayed\n*** End Patch\n", target.display());
    let call = ToolCall::function(
        "ambiguous-apply-patch".to_string(),
        "apply_patch".to_string(),
        serde_json::json!({"patch": patch}).to_string(),
    );
    let mut listing = archived_listing("ambiguous-legacy", temp.path(), "resume the edit");
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(String::new(), vec![call])),
        SessionMessage::from(&Message::tool_response(
            "ambiguous-apply-patch".to_string(),
            "first terminal result".to_string(),
        )),
        SessionMessage::from(&Message::tool_response(
            "ambiguous-apply-patch".to_string(),
            "conflicting terminal result".to_string(),
        )),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive_path = listing.path.clone();
    let original_archive = serde_json::to_vec(&listing.snapshot).unwrap();
    fs::write(&archive_path, &original_archive).unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = acp::SessionId::new("ambiguous-legacy-session");

    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path()).await.unwrap(),
            listing,
        )
        .await
    {
        Ok(_) => panic!("ambiguous tool results must not resume an ACP session"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("ambiguous tool-result history"));
    assert!(
        agent.session_handle(&session_id).unwrap().is_none(),
        "an ambiguous archive must not become a provider/tool continuation session"
    );
    assert_eq!(fs::read(&archive_path).unwrap(), original_archive);
    assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
}

#[tokio::test]
async fn repeated_call_ids_in_separate_completed_batches_remain_resumable() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().unwrap();
    let mut listing = archived_listing("reused-call-id", temp.path(), "continue");
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "reused-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response("reused-call".to_string(), "first result".to_string())),
        SessionMessage::from(&Message::user("continue".to_string())),
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "reused-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response("reused-call".to_string(), "second result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let agent = build_agent(temp.path()).await;
    let session_id = acp::SessionId::new("reused-call-id-session");

    let handle = agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path()).await.unwrap(),
            listing,
        )
        .await
        .expect("separate completed batches must remain resumable");

    assert!(agent.session_handle(&session_id).unwrap().is_some());
    assert_eq!(handle.data.lock().unwrap().thread.messages().len(), 5);
}
