//! Archive behaviour for ACP session state.
use super::super::archive::{WorkspaceMatch, session_listing_workspace_match, trace_storage_operation};
use super::*;
use assert_fs::TempDir;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};
use vtcode_core::llm::provider::MessageRole;

#[derive(Clone, Default)]
struct TraceCapture(Arc<Mutex<Vec<String>>>);

impl TraceCapture {
    fn record_storage_fields(&self, span_name: &str, record: impl FnOnce(&mut CapturedFields)) {
        if span_name != "acp_session_storage" {
            return;
        }

        let mut fields = CapturedFields::default();
        record(&mut fields);
        self.0
            .lock()
            .expect("trace capture lock")
            .push(format!("span={span_name}{}", fields.0));
    }
}

impl<S> Layer<S> for TraceCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, _id: &tracing::Id, _context: Context<'_, S>) {
        self.record_storage_fields(attrs.metadata().name(), |fields| attrs.record(fields));
    }

    fn on_record(&self, id: &tracing::Id, values: &tracing::span::Record<'_>, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        self.record_storage_fields(span.name(), |fields| values.record(fields));
    }

    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let Some(span) = context.lookup_current() else { return };
        if span.name() != "acp_session_storage" {
            return;
        }
        let mut fields = CapturedFields::default();
        event.record(&mut fields);
        self.0.lock().expect("trace capture lock").push(format!(
            "span={} event={}{}",
            span.name(),
            event.metadata().name(),
            fields.0
        ));
    }
}

#[derive(Default)]
struct CapturedFields(String);

impl Visit for CapturedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }
}

fn assert_storage_trace(captured: &str, operation: &str, outcome: &str, forbidden_values: &[&str]) {
    let operation_field = format!("operation={operation:?}");
    let outcome_field = format!("outcome={outcome:?}");
    let required_fields = [
        "span=acp_session_storage",
        operation_field.as_str(),
        outcome_field.as_str(),
        "elapsed_ms=",
    ];
    let missing_fields = required_fields
        .into_iter()
        .filter(|field| !captured.contains(field))
        .collect::<Vec<_>>();
    let leaked_values = forbidden_values
        .iter()
        .copied()
        .filter(|value| captured.contains(value))
        .collect::<Vec<_>>();

    assert!(
        missing_fields.is_empty() && leaked_values.is_empty(),
        "storage trace contract failed: missing={missing_fields:?}, leaked={leaked_values:?}, trace={captured}"
    );
}

#[derive(Debug, PartialEq)]
struct ListedSessionProjection {
    session_id: acp::SessionId,
    cwd: std::path::PathBuf,
    title: Option<String>,
}

#[derive(Debug, PartialEq)]
struct SessionListProjection<TCursor> {
    sessions: Vec<ListedSessionProjection>,
    next_cursor: TCursor,
}

pub(super) fn archived_listing(identifier: &str, workspace: &Path, prompt: &str) -> SessionListing {
    session_listing(
        identifier,
        workspace,
        SessionArchiveMetadata::new("workspace", workspace.to_string_lossy(), "gpt-5.4", "openai", "test", "low"),
        vec![SessionMessage::new(MessageRole::User, prompt)],
    )
}

#[tokio::test]
async fn storage_operation_records_failure_without_error_details() {
    let capture = TraceCapture::default();
    let subscriber = Registry::default().with(capture.clone());
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let result =
        trace_storage_operation("archive_attach", async { Err::<(), _>(anyhow::anyhow!("sensitive path and token")) })
            .await;

    let captured = capture.0.lock().expect("trace capture lock").join("\n");
    assert!(result.is_err());
    assert_storage_trace(&captured, "archive_attach", "failure", &["sensitive path", "token"]);
}

#[tokio::test]
async fn build_session_handle_restores_session_config_from_thread_metadata() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let listing = session_listing(
        "session-vtcode-acp-archive",
        temp.path(),
        SessionArchiveMetadata::new("vtcode", temp.path().to_string_lossy(), "gpt-5.6-sol", "openai", "test", "xhigh")
            .with_primary_agent("build"),
        Vec::new(),
    );
    let thread = agent
        .thread_manager
        .start_thread_with_identifier("session-vtcode-acp-archive", ThreadBootstrap::from_listing(listing));

    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            subagent_controller: None,
            session_id: acp::SessionId::new("session-1"),
            archive: None,
            trigger: SessionStartTrigger::NewSession,
            workspace_runtime: None,
            workspace_lease: None,
        },
    );

    assert_eq!(
        session_config_projection(&handle),
        SessionConfigProjection {
            primary_agent: "build".to_string(),
            reasoning_effort: ReasoningEffortLevel::XHigh,
            provider: "openai".to_string(),
            model: "gpt-5.6-sol".to_string(),
        }
    );
}

#[tokio::test]
async fn checkpoint_session_persists_messages_for_a_fresh_runtime() {
    let capture = TraceCapture::default();
    let subscriber = Registry::default().with(capture.clone());
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let path = temp.path().join("vtcode-zed-session-resume.json");
    let metadata =
        SessionArchiveMetadata::new("vtcode", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low")
            .with_primary_agent("build");
    let listing = session_listing("vtcode-zed-session-resume", temp.path(), metadata.clone(), Vec::new());
    let archive = SessionArchive::resume_from_listing(&listing, metadata);
    let thread = agent
        .thread_manager
        .start_thread_with_identifier("vtcode-zed-session-resume", ThreadBootstrap::from_listing(listing));
    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            subagent_controller: None,
            session_id: acp::SessionId::new("vtcode-zed-session-resume"),
            archive: Some(archive),
            trigger: SessionStartTrigger::Resume,
            workspace_runtime: None,
            workspace_lease: None,
        },
    );
    agent.push_message(&handle, Message::user("continue".to_string())).unwrap();
    agent
        .push_message(&handle, Message::assistant("resumed response".to_string()))
        .unwrap();

    agent.checkpoint_session(&handle).await.unwrap();

    let captured = capture.0.lock().expect("trace capture lock").join("\n");
    assert_storage_trace(
        &captured,
        "checkpoint",
        "success",
        &["vtcode-zed-session-resume.json", "continue", "api_key"],
    );

    let raw = fs::read_to_string(&path).unwrap();
    let snapshot: SessionSnapshot = serde_json::from_str(&raw).unwrap();
    let reloaded = SessionListing { path, snapshot };
    let fresh_thread = agent
        .thread_manager
        .start_thread_with_identifier("vtcode-zed-session-fresh-runtime", ThreadBootstrap::from_listing(reloaded));
    assert_eq!(
        fresh_thread
            .messages()
            .iter()
            .map(|message| message.content.as_text())
            .collect::<Vec<_>>(),
        ["continue", "resumed response"]
    );
}

#[test]
fn archived_workspace_matching_distinguishes_resolution_mismatch_and_match() {
    let matching_workspace = TempDir::new().unwrap();
    let different_workspace = TempDir::new().unwrap();
    let missing_workspace = matching_workspace.path().join("missing");
    let requested_workspace = vtcode_commons::canonicalize(matching_workspace.path()).unwrap();

    let matching = archived_listing("session-matching", matching_workspace.path(), "matching task");
    let different = archived_listing("session-different", different_workspace.path(), "different task");
    let unavailable = archived_listing("session-unavailable", &missing_workspace, "unavailable task");

    assert_eq!(session_listing_workspace_match(&matching, &requested_workspace).unwrap(), WorkspaceMatch::Matches);
    assert_eq!(session_listing_workspace_match(&different, &requested_workspace).unwrap(), WorkspaceMatch::Differs);
    let error = session_listing_workspace_match(&unavailable, &requested_workspace).unwrap_err();
    let formatted_error = format!("{error:#}");
    assert!(formatted_error.contains("resolving stored ACP session workspace"));
    assert!(!formatted_error.contains(&missing_workspace.display().to_string()));
}

#[tokio::test]
async fn active_session_attachment_distinguishes_resolution_mismatch_and_match() {
    let workspace = TempDir::new().unwrap();
    let different_workspace = TempDir::new().unwrap();
    let agent = build_agent(workspace.path()).await;
    let response = agent.new_session(acp::NewSessionRequest::new(workspace.path())).await.unwrap();
    let canonical_workspace = AcpSessionWorkspace::from_client_cwd(workspace.path()).await.unwrap();

    assert!(
        agent
            .attach_or_get_session(&response.session_id, &canonical_workspace)
            .await
            .is_ok()
    );
    let mismatch = match agent
        .attach_or_get_session(
            &response.session_id,
            &AcpSessionWorkspace::from_client_cwd(different_workspace.path()).await.unwrap(),
        )
        .await
    {
        Ok(_) => panic!("active session must reject a different workspace"),
        Err(error) => error,
    };
    assert!(mismatch.to_string().contains("different workspace"));

    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    {
        let data = session.data.lock().unwrap();
        let mut metadata = data.thread.metadata().unwrap();
        metadata.workspace_path = workspace.path().join("missing").to_string_lossy().into_owned();
        data.thread.replace_metadata(Some(metadata));
    }

    let unresolved = match agent.attach_or_get_session(&response.session_id, &canonical_workspace).await {
        Ok(_) => panic!("active session must reject an unresolved stored workspace"),
        Err(error) => error,
    };
    assert!(format!("{unresolved:#}").contains("resolving stored ACP session workspace"));
}

#[test]
fn session_list_filters_archives_by_workspace() {
    let first_workspace = TempDir::new().unwrap();
    let second_workspace = TempDir::new().unwrap();
    let workspace_alias = first_workspace.path().join(".");
    let request = acp::ListSessionsRequest::new().cwd(&workspace_alias);

    let response = ZedAgent::session_list_response(
        &request,
        vec![
            archived_listing("session-first", first_workspace.path(), "First task"),
            archived_listing("session-second", second_workspace.path(), "Second task"),
        ],
    )
    .unwrap();

    let observed = SessionListProjection {
        sessions: response
            .sessions
            .iter()
            .map(|session| ListedSessionProjection {
                session_id: session.session_id.clone(),
                cwd: session.cwd.clone(),
                title: session.title.clone(),
            })
            .collect(),
        next_cursor: response.next_cursor.clone(),
    };
    assert_eq!(
        observed,
        SessionListProjection {
            sessions: vec![ListedSessionProjection {
                session_id: acp::SessionId::new("session-first"),
                cwd: first_workspace.path().to_path_buf(),
                title: Some("First task".to_string()),
            }],
            next_cursor: None,
        }
    );
}

#[test]
fn session_list_skips_an_unresolvable_archive_in_workspace_filtered_results() {
    let workspace = TempDir::new().unwrap();
    let missing_workspace = workspace.path().join("missing");
    let request = acp::ListSessionsRequest::new().cwd(workspace.path());

    let response = ZedAgent::session_list_response(
        &request,
        vec![
            archived_listing("session-unavailable", &missing_workspace, "unavailable task"),
            archived_listing("session-available", workspace.path(), "available task"),
        ],
    )
    .expect("an unresolved stored workspace must not fail a filtered listing");

    assert_eq!(response.sessions.len(), 1);
    assert_eq!(response.sessions[0].session_id, acp::SessionId::new("session-available"));
    assert_eq!(response.sessions[0].cwd, workspace.path());
}

#[test]
fn session_list_rejects_unknown_cursor() {
    let request = acp::ListSessionsRequest::new().cursor("not-a-vtcode-cursor");
    assert!(ZedAgent::session_list_response(&request, Vec::new()).is_err());
}

#[test]
fn session_list_reports_an_invalid_requested_workspace() {
    let workspace = TempDir::new().unwrap();
    let missing_workspace = workspace.path().join("missing");
    let request = acp::ListSessionsRequest::new().cwd(&missing_workspace);
    let listings = vec![archived_listing("session-first", workspace.path(), "First task")];

    assert!(ZedAgent::session_list_response(&request, listings).is_err());
}

#[test]
fn session_list_cursor_advances_to_the_next_page() {
    let workspace = Path::new("/workspace/page");
    let listings = (0..=ZedAgent::SESSION_LIST_PAGE_SIZE)
        .map(|index| archived_listing(&format!("session-{index}"), workspace, "task"))
        .collect::<Vec<_>>();

    let first = ZedAgent::session_list_response(&acp::ListSessionsRequest::new(), listings.clone()).unwrap();
    assert_eq!(first.sessions.len(), ZedAgent::SESSION_LIST_PAGE_SIZE);
    let cursor = first.next_cursor.expect("first page should provide a cursor");

    let second = ZedAgent::session_list_response(&acp::ListSessionsRequest::new().cursor(cursor), listings).unwrap();
    assert_eq!(second.sessions.len(), 1);
    assert!(second.next_cursor.is_none());
}

use super::super::archive::repair_archived_tool_calls;
use vtcode_core::core::threads::messages_from_session_listing;
use vtcode_core::llm::provider::ToolCall;

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
            "legacy-mutating",
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
            "repair-checkpoint-failure",
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
            "ambiguous-legacy",
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
            "reused-call-id",
            &AcpSessionWorkspace::from_client_cwd(temp.path()).await.unwrap(),
            listing,
        )
        .await
        .expect("separate completed batches must remain resumable");

    assert!(agent.session_handle(&session_id).unwrap().is_some());
    assert_eq!(handle.data.lock().unwrap().thread.messages().len(), 5);
}
