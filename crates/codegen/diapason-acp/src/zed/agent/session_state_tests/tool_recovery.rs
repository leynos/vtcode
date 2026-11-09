//! Tool-call repair when an archived ACP session is resumed.
//!
//! Covers placeholder insertion for legacy archives, relocation of a uniquely
//! attributable late result, refusal of ambiguous history, and the durable
//! checkpoint that must precede registration.
use super::super::archive::repair_archived_tool_calls;
use super::archive::archived_listing;
use super::*;
use crate::zed::agent::session_state::{CheckpointFailure, RequiredCheckpointError};
use crate::zed::session::AcpSessionWorkspace;
use assert_fs::TempDir;
use diapason_core::core::threads::messages_from_session_listing;
use diapason_core::llm::provider::{MessageContent, MessageRole, ToolCall};

/// Closes a legacy tool call with an incomplete result before a later user
/// message.
#[test]
fn legacy_archive_repair_closes_tool_call_before_later_user_message() {
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
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

/// Persists a relocation-only repair to the archive.
#[test]
fn relocation_only_repair_is_persisted_to_the_archive() {
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
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

/// Resumes when front compaction leaves tool results without their assistant
/// request.
#[test]
fn truncated_archive_prefix_resumes_without_ambiguity() {
    // Front compaction can retain tool results whose assistant request was
    // trimmed away; that shape must resume rather than being rejected.
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
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

/// Repairs a legacy archive on attach without replaying its mutating call.
#[tokio::test]
async fn attaching_legacy_archive_repairs_without_replaying_mutating_call() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
    let target = temp.path().join("must-not-change.txt");
    fs::write(&target, "original\n").expect("seed the file the replayed patch would modify");
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
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
        .expect("attach the legacy archive without replaying its mutating call");

    assert_eq!(fs::read_to_string(&target).expect("read the file the replayed patch would modify"), "original\n");
    let messages = handle.data.lock().expect("lock the attached session data").thread.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert_eq!(messages[1].role, MessageRole::Tool);
    let recovery = messages[1].content.as_text();
    assert!(recovery.contains("\"replayed\":false"));
    assert!(recovery.contains("Verify the current workspace state"));
    assert!(recovery.contains("resubmit the tool call only if"));

    let persisted: SessionSnapshot =
        serde_json::from_str(&fs::read_to_string(archive_path).expect("read the repaired archive after attach"))
            .expect("parse the repaired archive snapshot");
    assert_eq!(persisted.messages.len(), 2);
    assert_eq!(persisted.messages[1].role, MessageRole::Tool);
}

/// Leaves the session unregistered when the repair checkpoint fails.
#[tokio::test]
async fn repaired_archive_is_not_registered_when_repair_checkpoint_fails() {
    // Reviews 3791762677 and 3944816962: a resumed session is visible to
    // callers only after its repaired history has been durably written.
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
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
    fs::create_dir(&listing.path).expect("occupy the archive path with a directory");

    let session_id = acp::SessionId::new("repair-checkpoint-failure-session");
    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a directory at the archive path must make the repair checkpoint fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("Failed to persist repaired ACP tool-call history"));
    assert!(
        agent
            .session_handle(&session_id)
            .expect("query the session registry after the failed repair checkpoint")
            .is_none(),
        "failed repair checkpoints must not leave a partially registered session"
    );
}

/// A repair checkpoint fails when compaction drops its repaired evidence.
///
/// The staged batch alone is not enough: this archive's last batch survives
/// compaction, and only the repaired earlier call is trimmed away. The
/// pre-write gate rejects the payload, so the archive already on disk keeps its
/// exact bytes and a retry fails again.
#[tokio::test]
async fn repair_checkpoint_fails_when_compaction_drops_repaired_evidence() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    // The budget fits the last assistant batch and its result but not the
    // oversized repaired call, so front trimming removes exactly that call.
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("create temporary workspace for the repair compaction test");
    let mut listing = archived_listing("repair-compaction", temp.path(), "continue");
    listing.snapshot.messages = repaired_evidence_under_compaction(32 * 1024);
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive_path = listing.path.clone();
    // The interrupted session already has an archive from its last checkpoint.
    // It is over this run's budget, which is exactly why the repaired payload
    // cannot retain the evidence.
    let existing_archive = serde_json::to_vec(&listing.snapshot).expect("serialize the existing archive");
    fs::write(&archive_path, &existing_archive).expect("seed the existing repaired-session archive");
    let agent = build_agent(temp.path()).await;

    let error = match agent
        .attach_thread_from_listing(
            &acp::SessionId::new("repair-compaction-session"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a repair checkpoint that dropped its evidence must fail"),
        Err(error) => error,
    };

    assert!(
        format!("{error:#}").contains("dropped 1 staged tool call"),
        "the failure must name the dropped repaired evidence: {error:#}"
    );
    assert_eq!(
        error
            .downcast_ref::<RequiredCheckpointError>()
            .map(RequiredCheckpointError::failure),
        Some(CheckpointFailure::RetentionDropped),
        "dropped repaired evidence must carry the bounded retention category"
    );
    assert_eq!(
        fs::read(&archive_path).expect("read the existing archive after the rejected checkpoint"),
        existing_archive,
        "a rejected repair checkpoint must leave the existing archive bytes unchanged"
    );

    // A retry must fail too: because the rejected checkpoint wrote nothing,
    // the second attempt replays the same repair from the preserved archive
    // bytes and rejects it again rather than resuming from a compacted archive
    // that lost the repaired evidence.
    let preserved: SessionSnapshot =
        serde_json::from_slice(&fs::read(&archive_path).expect("read the preserved archive before the retry"))
            .expect("parse the preserved archive before the retry");
    let mut retry_listing = archived_listing("repair-compaction", temp.path(), "continue");
    retry_listing.snapshot = preserved;
    retry_listing.snapshot.total_messages = retry_listing.snapshot.messages.len();
    let retry = agent
        .attach_thread_from_listing(
            &acp::SessionId::new("repair-compaction-session-retry"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            retry_listing,
        )
        .await;
    assert!(
        retry.is_err(),
        "a retry from the unchanged archive must fail instead of resuming without the repaired evidence"
    );
    assert_eq!(
        fs::read(&archive_path).expect("read the existing archive after the retry"),
        existing_archive,
        "a retried rejected checkpoint must leave the existing archive bytes unchanged"
    );
}

/// A repair checkpoint fails when compaction drops only its repaired request.
///
/// The later completed batch reuses the repaired call id, so the ordered per-id
/// check and the batch check both still pass, and the repaired placeholder keeps
/// its own slot. Only the request's position shows that the batch which owned
/// the repaired result was trimmed away.
#[tokio::test]
async fn repair_checkpoint_fails_when_compaction_drops_the_repaired_request() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    // The budget fits the later batch and its result but not the oversized
    // repaired request, so front trimming removes exactly that request.
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("create temporary workspace for the repaired-request test");
    let mut listing = archived_listing("repair-request-compaction", temp.path(), "continue");
    listing.snapshot.messages = reused_id_repaired_request_under_compaction(32 * 1024);
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive_path = listing.path.clone();
    let existing_archive = serde_json::to_vec(&listing.snapshot).expect("serialize the existing archive");
    fs::write(&archive_path, &existing_archive).expect("seed the existing repaired-session archive");
    let agent = build_agent(temp.path()).await;

    let error = match agent
        .attach_thread_from_listing(
            &acp::SessionId::new("repair-request-compaction-session"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a repair checkpoint that dropped its repaired request must fail"),
        Err(error) => error,
    };

    assert!(
        format!("{error:#}").contains("dropped repaired evidence for 1 tool call"),
        "the failure must name the dropped repaired request: {error:#}"
    );
    assert_eq!(
        error
            .downcast_ref::<RequiredCheckpointError>()
            .map(RequiredCheckpointError::failure),
        Some(CheckpointFailure::RetentionDropped),
        "dropped repaired evidence must carry the bounded retention category"
    );
    assert_eq!(
        fs::read(&archive_path).expect("read the existing archive after the rejected checkpoint"),
        existing_archive,
        "a rejected repair checkpoint must leave the existing archive bytes unchanged"
    );

    // A retry from the preserved archive must fail again for the same reason.
    let preserved: SessionSnapshot =
        serde_json::from_slice(&fs::read(&archive_path).expect("read the preserved archive before the retry"))
            .expect("parse the preserved archive before the retry");
    let mut retry_listing = archived_listing("repair-request-compaction", temp.path(), "continue");
    retry_listing.snapshot = preserved;
    retry_listing.snapshot.total_messages = retry_listing.snapshot.messages.len();
    let retry = agent
        .attach_thread_from_listing(
            &acp::SessionId::new("repair-request-compaction-session-retry"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            retry_listing,
        )
        .await;
    assert!(
        retry.is_err(),
        "a retry from the unchanged archive must fail instead of resuming without the repaired request"
    );
    assert_eq!(
        fs::read(&archive_path).expect("read the existing archive after the retry"),
        existing_archive,
        "a retried rejected checkpoint must leave the existing archive bytes unchanged"
    );
}

/// A retained repaired request is not reported lost when an earlier identical
/// batch was trimmed away.
///
/// Both batches request the same call id with an empty body, so searching the
/// history by content selects the earlier request. Retention must check the
/// position the repair recorded; otherwise a checkpoint whose own evidence
/// survived is rejected.
#[tokio::test]
async fn repair_checkpoint_retains_a_request_after_an_earlier_identical_batch() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("create temporary workspace for the earlier-identical-batch test");
    let mut listing = archived_listing("repair-earlier-identical-batch", temp.path(), "continue");
    listing.snapshot.messages = identical_completed_batch_before_the_repaired_request(32 * 1024);
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive_path = listing.path.clone();
    let agent = build_agent(temp.path()).await;

    let handle = agent
        .attach_thread_from_listing(
            &acp::SessionId::new("repair-earlier-identical-batch-session"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
        .expect("a repaired request that survives compaction must not be reported lost");

    let persisted: SessionSnapshot =
        serde_json::from_str(&fs::read_to_string(&archive_path).expect("read the repaired archive"))
            .expect("parse the repaired archive snapshot");
    assert_eq!(persisted.messages.len(), 2, "front compaction must trim the earlier completed batch");
    assert_eq!(persisted.messages[0].role, MessageRole::Assistant);
    assert_eq!(
        persisted.messages[0]
            .tool_calls
            .as_deref()
            .and_then(|calls| calls.first())
            .map(|call| call.id.as_str()),
        Some("reused-completed-call"),
        "the repaired request must be the surviving request"
    );
    assert_eq!(
        persisted.messages[1].tool_call_id.as_deref().map(String::as_str),
        Some("reused-completed-call"),
        "the repaired placeholder must be the surviving result"
    );
    let messages = handle.data.lock().expect("lock the attached session data").thread.messages();
    assert!(
        messages
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("reused-completed-call")),
        "the retained placeholder must remain in the live history"
    );
}

/// A repair checkpoint retains a relocated result whose content is empty.
///
/// A tool that returned no output is legitimate evidence, so retention must
/// accept the unchanged empty content rather than wait for compaction.
#[tokio::test]
async fn repair_checkpoint_retains_an_empty_relocated_result() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("create temporary workspace for the empty-result test");
    let mut listing = archived_listing("empty-relocated-result", temp.path(), "continue");
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "empty-result-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::user("continue".to_string())),
        SessionMessage::from(&Message::tool_response("empty-result-call".to_string(), String::new())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive_path = listing.path.clone();
    let agent = build_agent(temp.path()).await;

    let handle = agent
        .attach_thread_from_listing(
            &acp::SessionId::new("empty-relocated-result-session"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
        .expect("an empty relocated result must not fail retention");

    let messages = handle.data.lock().expect("lock the attached session data").thread.messages();
    assert_eq!(messages[1].role, MessageRole::Tool);
    assert_eq!(messages[1].tool_call_id.as_deref(), Some("empty-result-call"));
    assert_eq!(messages[1].content.as_text(), "");

    let persisted: SessionSnapshot =
        serde_json::from_str(&fs::read_to_string(&archive_path).expect("read the repaired archive"))
            .expect("parse the repaired archive snapshot");
    assert_eq!(persisted.messages[1].role, MessageRole::Tool);
    assert_eq!(
        persisted.messages[1].tool_call_id.as_deref().map(String::as_str),
        Some("empty-result-call"),
        "the retained archive must keep the relocated empty result"
    );
    assert_eq!(persisted.messages[1].content, MessageContent::Text(String::new()));
}

/// A repair checkpoint succeeds when compaction retains the repaired call.
#[tokio::test]
async fn repair_checkpoint_succeeds_when_compaction_retains_repaired_evidence() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("create temporary workspace for the repair retention test");
    let mut listing = archived_listing("repair-retained", temp.path(), "continue");
    listing.snapshot.messages = repaired_evidence_under_compaction(0);
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive_path = listing.path.clone();
    let agent = build_agent(temp.path()).await;

    let handle = agent
        .attach_thread_from_listing(
            &acp::SessionId::new("repair-retained-session"),
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
        .expect("a repaired batch that survives compaction must attach");

    let messages = handle.data.lock().expect("lock the attached session data").thread.messages();
    assert!(
        messages
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("repaired-evidence-call")),
        "the repaired placeholder must remain in the live history"
    );
    let persisted: SessionSnapshot =
        serde_json::from_str(&fs::read_to_string(archive_path).expect("read the repaired archive"))
            .expect("parse the repaired archive snapshot");
    assert!(
        persisted
            .messages
            .iter()
            .any(|message| message.tool_call_id.as_deref().is_some_and(|id| id == "repaired-evidence-call")),
        "the repaired placeholder must remain in the persisted archive"
    );
}

/// Builds an archive with an earlier repaired call and a later completed batch.
///
/// `padding` lengthens the repaired call's arguments so front compaction must
/// trim that first message while the later batch still fits the budget.
fn repaired_evidence_under_compaction(padding: usize) -> Vec<SessionMessage> {
    vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "repaired-evidence-call".to_string(),
                "read_file".to_string(),
                format!("{{\"path\":\"{}\"}}", "x".repeat(padding)),
            )],
        )),
        SessionMessage::from(&Message::user("continue".to_string())),
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "later-batch-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response("later-batch-call".to_string(), "later result".to_string())),
    ]
}

/// Builds an archive whose repaired batch and a later batch reuse a call id.
///
/// `padding` lengthens the repaired request's arguments so front compaction
/// trims that request while the later completed batch still fits the budget.
fn reused_id_repaired_request_under_compaction(padding: usize) -> Vec<SessionMessage> {
    vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "reused-evidence-call".to_string(),
                "read_file".to_string(),
                format!("{{\"path\":\"{}\"}}", "x".repeat(padding)),
            )],
        )),
        SessionMessage::from(&Message::user("continue".to_string())),
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "reused-evidence-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response("reused-evidence-call".to_string(), "later result".to_string())),
    ]
}

/// Builds an archive whose repaired batch follows an identical completed one.
///
/// `padding` oversizes both the earlier request's arguments and its result, so
/// front compaction trims that whole earlier batch while the repaired request
/// and the placeholder the repair inserts still fit the budget. The two
/// requests are indistinguishable by role, content and call id, so only the
/// position the repair records identifies the repaired one.
fn identical_completed_batch_before_the_repaired_request(padding: usize) -> Vec<SessionMessage> {
    vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "reused-completed-call".to_string(),
                "read_file".to_string(),
                format!("{{\"path\":\"{}\"}}", "x".repeat(padding)),
            )],
        )),
        SessionMessage::from(&Message::tool_response("reused-completed-call".to_string(), "x".repeat(padding))),
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "reused-completed-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
    ]
}

/// Rejects an ambiguous archive without rewriting it or replaying its call.
#[tokio::test]
async fn ambiguous_archive_is_not_resumed_or_replayed_and_preserves_the_archive() {
    // A duplicate result has a known external-effect record, but no safe
    // canonical provider continuation. Reject before the archive can be
    // rewritten or the session becomes eligible for a prompt/tool call.
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
    let target = temp.path().join("must-not-change.txt");
    fs::write(&target, "original\n").expect("seed the file the replayed patch would modify");
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
    let original_archive = serde_json::to_vec(&listing.snapshot).expect("serialize the ambiguous archive");
    fs::write(&archive_path, &original_archive).expect("write the ambiguous archive to disk");
    let agent = build_agent(temp.path()).await;
    let session_id = acp::SessionId::new("ambiguous-legacy-session");

    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("ambiguous tool results must not resume an ACP session"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("ambiguous tool-result history"));
    assert!(
        agent
            .session_handle(&session_id)
            .expect("query the session registry after the ambiguous archive rejection")
            .is_none(),
        "an ambiguous archive must not become a provider/tool continuation session"
    );
    assert_eq!(fs::read(&archive_path).expect("read the archive after the ambiguous rejection"), original_archive);
    assert_eq!(fs::read_to_string(&target).expect("read the file the replayed patch would modify"), "original\n");
}

/// Resumes separate completed batches that reuse the same call ID.
#[tokio::test]
async fn repeated_call_ids_in_separate_completed_batches_remain_resumable() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for tool-recovery test");
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
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
        .expect("separate completed batches must remain resumable");

    assert!(
        agent
            .session_handle(&session_id)
            .expect("query the session registry after resuming reused call IDs")
            .is_some()
    );
    assert_eq!(
        handle
            .data
            .lock()
            .expect("lock the resumed session data")
            .thread
            .messages()
            .len(),
        5
    );
}
