//! Archive behaviour for ACP session state.
use super::super::archive::{
    CheckpointDurability, CheckpointReader, SessionArchiveCheckpointReader, WorkspaceMatch,
    missing_persisted_tool_calls, required_checkpoint_retention_violation, session_listing_workspace_match,
    staged_tool_call_ids, trace_storage_operation, verify_required_checkpoint_retention,
};
use super::*;
use crate::zed::agent::session_state::{CheckpointFailure, RequiredCheckpointError};
use assert_fs::TempDir;
use diapason_core::llm::provider::{MessageRole, ToolCall};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

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

/// Builds a minimal archived session listing for resume tests.
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
        "session-diapason-acp-archive",
        temp.path(),
        SessionArchiveMetadata::new(
            "diapason",
            temp.path().to_string_lossy(),
            "gpt-5.6-sol",
            "openai",
            "test",
            "xhigh",
        )
        .with_primary_agent("build"),
        Vec::new(),
    );
    let thread = agent
        .thread_manager
        .start_thread_with_identifier("session-diapason-acp-archive", ThreadBootstrap::from_listing(listing));

    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
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
    let path = temp.path().join("diapason-acp-session-resume.json");
    let metadata =
        SessionArchiveMetadata::new("diapason", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low")
            .with_primary_agent("build");
    let listing = session_listing("diapason-acp-session-resume", temp.path(), metadata.clone(), Vec::new());
    let archive = SessionArchive::resume_from_listing(&listing, metadata);
    let thread = agent
        .thread_manager
        .start_thread_with_identifier("diapason-acp-session-resume", ThreadBootstrap::from_listing(listing));
    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            session_id: acp::SessionId::new("diapason-acp-session-resume"),
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
        &["diapason-acp-session-resume.json", "continue", "api_key"],
    );

    let raw = fs::read_to_string(&path).unwrap();
    let snapshot: SessionSnapshot = serde_json::from_str(&raw).unwrap();
    let reloaded = SessionListing { path, snapshot };
    let fresh_thread = agent
        .thread_manager
        .start_thread_with_identifier("diapason-acp-session-fresh-runtime", ThreadBootstrap::from_listing(reloaded));
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
    let requested_workspace = diapason_commons::canonicalize(matching_workspace.path()).unwrap();

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
    let request = acp::ListSessionsRequest::new().cursor("not-a-diapason-cursor");
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

/// A required checkpoint must reject a snapshot compacted past its staged batch.
#[test]
fn required_checkpoint_rejects_a_snapshot_compacted_beyond_its_staged_calls() {
    let staged = vec!["staged-call-0001".to_string()];

    assert_eq!(
        required_checkpoint_retention_violation(CheckpointDurability::Required, &staged, &[]),
        Some(1),
        "an emptied snapshot must be rejected when durability is required"
    );
    assert_eq!(
        required_checkpoint_retention_violation(CheckpointDurability::BestEffort, &staged, &[]),
        None,
        "best-effort checkpoints tolerate compaction loss"
    );
}

/// A required checkpoint accepts a snapshot that retains both sides of the batch.
#[test]
fn required_checkpoint_accepts_a_snapshot_that_retains_the_staged_batch() {
    let staged = vec!["staged-call-0001".to_string()];
    let messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                staged[0].clone(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response(staged[0].clone(), "result".to_string())),
    ];

    assert!(missing_persisted_tool_calls(&staged, &messages).is_empty());
    assert_eq!(required_checkpoint_retention_violation(CheckpointDurability::Required, &staged, &messages), None);
}

/// A checkpoint reader that always fails to read the archive back.
struct FailingCheckpointReader;

impl CheckpointReader for FailingCheckpointReader {
    async fn read_persisted_messages(&self, _persisted_path: &Path) -> anyhow::Result<Vec<SessionMessage>> {
        Err(anyhow::anyhow!("checkpoint storage unavailable"))
    }
}

/// A required checkpoint fails and retains the cause when its read-back fails.
#[tokio::test]
async fn required_checkpoint_propagates_a_checkpoint_read_failure() {
    let staged = vec!["staged-read-failure-call".to_string()];

    let error = verify_required_checkpoint_retention(
        &FailingCheckpointReader,
        Path::new("/nonexistent/checkpoint.json"),
        &staged,
        &staged,
        &[],
    )
    .await
    .expect_err("a storage failure must fail the required checkpoint");

    assert!(
        format!("{error:#}").contains("checkpoint storage unavailable"),
        "the reader's storage failure must be retained: {error:#}"
    );
}

/// The production reader verifies retention without rewriting the archive.
#[tokio::test]
async fn required_checkpoint_reader_leaves_the_persisted_archive_unchanged() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let temp = TempDir::new().expect("temporary directory for the read-only checkpoint test");
    let session_dir = temp.path().join("sessions");
    fs::create_dir_all(&session_dir).expect("create the isolated ACP session directory");
    let _session_dir_guard = SessionDirGuard::set(&session_dir);

    let metadata =
        SessionArchiveMetadata::new("diapason", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low");
    let staged_id = "staged-read-only-call-0001".to_string();
    let listing = session_listing(
        "read-only-identifier",
        temp.path(),
        metadata,
        vec![
            SessionMessage::from(&Message::assistant_with_tools(
                String::new(),
                vec![ToolCall::function(
                    staged_id.clone(),
                    "read_file".to_string(),
                    "{}".to_string(),
                )],
            )),
            SessionMessage::from(&Message::tool_response(staged_id.clone(), "result".to_string())),
        ],
    );
    // The file stem differs from the identifier, so the reader must fall back
    // to this exact path rather than resolving through the session listing.
    let archive_path = temp.path().join("checkpoint-read-only.json");
    diapason_commons::fs::write_private_json_file(&archive_path, &listing.snapshot)
        .await
        .expect("persist the checkpoint archive");
    let before = fs::read(&archive_path).expect("read the archive before verification");

    verify_required_checkpoint_retention(
        &SessionArchiveCheckpointReader,
        &archive_path,
        std::slice::from_ref(&staged_id),
        std::slice::from_ref(&staged_id),
        &[],
    )
    .await
    .expect("a retained staged batch must verify");

    let after = fs::read(&archive_path).expect("read the archive after verification");
    assert_eq!(before, after, "verification must not rewrite the persisted archive");
}

/// The production reader fails closed on a missing archive without creating one.
#[tokio::test]
async fn production_checkpoint_reader_reports_a_missing_archive() {
    let temp = TempDir::new().expect("temporary directory for the missing-reader test");
    let missing_path = temp.path().join("absent-checkpoint.json");

    let error = SessionArchiveCheckpointReader
        .read_persisted_messages(&missing_path)
        .await
        .expect_err("reading a missing checkpoint must fail");
    assert!(
        format!("{error:#}").contains("reading back the required ACP session checkpoint file"),
        "the read-back failure must carry its operation context: {error:#}"
    );
    assert!(!missing_path.exists(), "a failed read must not create the archive");
    assert_eq!(
        fs::read_dir(temp.path()).expect("list the archive directory").count(),
        0,
        "the production reader must not write anything into the session directory"
    );
}

/// A storage failure at the production boundary is a read-back failure.
#[tokio::test]
async fn required_checkpoint_reports_a_production_read_back_failure() {
    let temp = TempDir::new().expect("temporary directory for the read-back category test");
    let missing_path = temp.path().join("absent-checkpoint.json");
    let staged = vec!["read-back-category-call".to_string()];

    let error =
        verify_required_checkpoint_retention(&SessionArchiveCheckpointReader, &missing_path, &staged, &staged, &[])
            .await
            .expect_err("a missing archive must fail the required checkpoint");

    let failure = error
        .downcast_ref::<RequiredCheckpointError>()
        .expect("the failure must carry its bounded category")
        .failure();
    assert_eq!(failure, CheckpointFailure::ReadBackFailed);
}

/// A best-effort checkpoint must not replace an archive whose compacted payload
/// lost the write-ahead evidence.
///
/// The checkpoint cannot fail the turn, so it skips the write and leaves the
/// last archive that still contains the staged request and result.
#[tokio::test]
async fn best_effort_checkpoint_preserves_write_ahead_evidence_under_compaction() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    // The small write-ahead batch fits; a large terminal result does not.
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("temporary directory for the write-ahead protection test");
    let session_dir = temp.path().join("sessions");
    fs::create_dir_all(&session_dir).expect("create the isolated ACP session directory");
    let _session_dir_guard = SessionDirGuard::set(&session_dir);
    let agent = build_agent(temp.path()).await;

    let identifier = "best-effort-write-ahead-protection";
    let metadata =
        SessionArchiveMetadata::new("diapason", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low");
    let staged_id = "staged-protected-call-0001".to_string();
    let mut listing = session_listing(identifier, temp.path(), metadata.clone(), Vec::new());
    let archive_path = session_dir.join(format!("{identifier}.json"));
    listing.path = archive_path.clone();
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                staged_id.clone(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response(staged_id.clone(), "incomplete result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive = SessionArchive::resume_from_listing(&listing, metadata);
    let thread = agent
        .thread_manager
        .start_thread_with_identifier(identifier, ThreadBootstrap::from_listing(listing));
    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            session_id: acp::SessionId::new(identifier),
            archive: Some(archive),
            trigger: SessionStartTrigger::Resume,
            workspace_runtime: None,
            workspace_lease: None,
        },
    );

    agent
        .checkpoint_session_required(&handle)
        .await
        .expect("the small write-ahead batch must persist");
    let before = fs::read(&archive_path).expect("read the write-ahead archive");
    assert_eq!(
        handle.data.lock().expect("session data lock").write_ahead_call_ids.clone(),
        vec![staged_id.clone()],
        "the required checkpoint must protect its staged call"
    );

    // A terminal result too large for the budget makes compaction trim the
    // assistant request away, so the prepared payload loses the request side
    // of the write-ahead evidence.
    let oversized_result = "r".repeat(32 * 1024);
    handle
        .data
        .lock()
        .expect("session data lock")
        .thread
        .mutate_messages(|messages| {
            for message in messages
                .iter_mut()
                .filter(|message| message.tool_call_id.as_deref() == Some(staged_id.as_str()))
            {
                *message = Message::tool_response(staged_id.clone(), oversized_result.clone());
            }
        });

    agent
        .checkpoint_session(&handle)
        .await
        .expect("a best-effort checkpoint must tolerate the loss by skipping the write");

    let after = fs::read(&archive_path).expect("read the archive after the skipped checkpoint");
    assert_eq!(before, after, "a skipped best-effort checkpoint must leave the archive unchanged");
    let persisted = find_session_by_identifier(identifier)
        .await
        .expect("read the persisted archive")
        .expect("the write-ahead archive must remain discoverable");
    assert_eq!(persisted.snapshot.messages.len(), 2, "the archived write-ahead request and result must survive");
    assert!(
        persisted.snapshot.messages.iter().any(|message| message
            .tool_calls
            .as_deref()
            .is_some_and(|calls| calls.iter().any(|call| call.id == staged_id))),
        "the archived write-ahead request must still be present"
    );
}

/// Compaction that drops the staged batch fails the required checkpoint.
///
/// A tool round only starts after a required checkpoint succeeds, so this
/// boundary is what prevents executing a call whose write-ahead evidence the
/// archive no longer retains.
#[tokio::test]
async fn required_checkpoint_fails_when_compaction_drops_the_staged_batch() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    // This budget fits the metadata and a lone tool result but not the
    // assistant batch, so compaction trims the request side away.
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("temporary directory for the compaction checkpoint test");
    let session_dir = temp.path().join("sessions");
    fs::create_dir_all(&session_dir).expect("create the isolated ACP session directory");
    let _session_dir_guard = SessionDirGuard::set(&session_dir);
    let agent = build_agent(temp.path()).await;

    let identifier = "required-checkpoint-compaction";
    let metadata =
        SessionArchiveMetadata::new("diapason", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low");
    let staged_id = "staged-compaction-call-0001".to_string();
    let oversized_arguments = "x".repeat(32 * 1024);
    let mut listing = session_listing(identifier, temp.path(), metadata.clone(), Vec::new());
    listing.path = session_dir.join(format!("{identifier}.json"));
    let archive_path = listing.path.clone();
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                staged_id.clone(),
                "read_file".to_string(),
                oversized_arguments,
            )],
        )),
        SessionMessage::from(&Message::tool_response(staged_id, "result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive = SessionArchive::resume_from_listing(&listing, metadata);
    let thread = agent
        .thread_manager
        .start_thread_with_identifier(identifier, ThreadBootstrap::from_listing(listing));
    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            session_id: acp::SessionId::new(identifier),
            archive: Some(archive),
            trigger: SessionStartTrigger::Resume,
            workspace_runtime: None,
            workspace_lease: None,
        },
    );

    let error = agent
        .checkpoint_session_required(&handle)
        .await
        .expect_err("a snapshot compacted past its staged batch must fail the required checkpoint");

    assert!(
        format!("{error:#}").contains("dropped 1 staged tool call"),
        "the failure must name the dropped staged batch: {error:#}"
    );
    assert_eq!(
        error
            .downcast_ref::<RequiredCheckpointError>()
            .map(RequiredCheckpointError::failure),
        Some(CheckpointFailure::RetentionDropped),
        "a dropped batch must carry the bounded retention category"
    );
    assert!(
        !archive_path.exists(),
        "a rejected required checkpoint must not write the compacted payload to disk"
    );

    // A retry must fail too: the rejected checkpoint cannot leave a compacted
    // archive that a later attempt would accept without the staged batch.
    let retry = agent
        .checkpoint_session_required(&handle)
        .await
        .expect_err("a retried required checkpoint must fail the same way");
    assert!(
        format!("{retry:#}").contains("dropped 1 staged tool call"),
        "the retried failure must still name the dropped staged batch: {retry:#}"
    );
}

/// A required checkpoint still succeeds when compaction retains the batch.
#[tokio::test]
async fn required_checkpoint_succeeds_when_compaction_retains_the_staged_batch() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    // The same budget as the loss case, but a small enough batch to survive.
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, Some(8 * 1024));
    let temp = TempDir::new().expect("temporary directory for the retained compaction test");
    let session_dir = temp.path().join("sessions");
    fs::create_dir_all(&session_dir).expect("create the isolated ACP session directory");
    let _session_dir_guard = SessionDirGuard::set(&session_dir);
    let agent = build_agent(temp.path()).await;

    let identifier = "required-checkpoint-retained";
    let metadata =
        SessionArchiveMetadata::new("diapason", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low");
    let staged_id = "staged-retained-call-0001".to_string();
    let mut listing = session_listing(identifier, temp.path(), metadata.clone(), Vec::new());
    listing.path = session_dir.join(format!("{identifier}.json"));
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                staged_id.clone(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response(staged_id, "result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive = SessionArchive::resume_from_listing(&listing, metadata);
    let thread = agent
        .thread_manager
        .start_thread_with_identifier(identifier, ThreadBootstrap::from_listing(listing));
    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            session_id: acp::SessionId::new(identifier),
            archive: Some(archive),
            trigger: SessionStartTrigger::Resume,
            workspace_runtime: None,
            workspace_lease: None,
        },
    );

    agent
        .checkpoint_session_required(&handle)
        .await
        .expect("a batch that survives compaction must verify");
}

/// Only the most recent assistant tool batch is treated as staged.
#[test]
fn staged_tool_call_ids_selects_the_last_assistant_batch() {
    let messages = vec![
        Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                "earlier-call".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        ),
        Message::tool_response("earlier-call".to_string(), "earlier result".to_string()),
        Message::assistant_with_tools(
            String::new(),
            vec![
                ToolCall::function("staged-call-a".to_string(), "read_file".to_string(), "{}".to_string()),
                ToolCall::function("staged-call-b".to_string(), "read_file".to_string(), "{}".to_string()),
            ],
        ),
    ];

    assert_eq!(staged_tool_call_ids(&messages), ["staged-call-a", "staged-call-b"]);
    assert!(staged_tool_call_ids(&[Message::user("no tools".to_string())]).is_empty());
}

/// Restores the process session directory for the guard's lifetime.
struct SessionDirGuard {
    previous: Option<std::ffi::OsString>,
    environment_lock: diapason_commons::env_lock::EnvGuard,
}

impl SessionDirGuard {
    fn set(path: &Path) -> Self {
        let environment_lock = diapason_commons::env_lock::lock();
        let previous = std::env::var_os(diapason_core::utils::session_archive::SESSION_DIR_ENV);
        environment_lock.set_var(diapason_core::utils::session_archive::SESSION_DIR_ENV, path.as_os_str());
        Self { previous, environment_lock }
    }
}

impl Drop for SessionDirGuard {
    fn drop(&mut self) {
        self.environment_lock
            .restore_var(diapason_core::utils::session_archive::SESSION_DIR_ENV, self.previous.as_ref());
    }
}

/// A required checkpoint reads its canonical archive back and retains the batch.
#[tokio::test]
async fn required_checkpoint_verifies_the_persisted_staged_batch() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().unwrap();
    let session_dir = temp.path().join("sessions");
    fs::create_dir_all(&session_dir).unwrap();
    let _session_dir_guard = SessionDirGuard::set(&session_dir);
    let agent = build_agent(temp.path()).await;
    let identifier = "required-checkpoint-retention";
    let metadata =
        SessionArchiveMetadata::new("diapason", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low");
    let staged_id = "staged-retention-call-0001".to_string();
    let mut listing = session_listing(identifier, temp.path(), metadata.clone(), Vec::new());
    listing.path = session_dir.join(format!("{identifier}.json"));
    listing.snapshot.messages = vec![
        SessionMessage::from(&Message::assistant_with_tools(
            String::new(),
            vec![ToolCall::function(
                staged_id.clone(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        )),
        SessionMessage::from(&Message::tool_response(staged_id, "result".to_string())),
    ];
    listing.snapshot.total_messages = listing.snapshot.messages.len();
    let archive = SessionArchive::resume_from_listing(&listing, metadata);
    let thread = agent
        .thread_manager
        .start_thread_with_identifier(identifier, ThreadBootstrap::from_listing(listing));
    let handle = agent.build_session_handle(
        thread,
        SessionHandleBootstrap {
            session_id: acp::SessionId::new(identifier),
            archive: Some(archive),
            trigger: SessionStartTrigger::Resume,
            workspace_runtime: None,
            workspace_lease: None,
        },
    );

    agent
        .checkpoint_session_required(&handle)
        .await
        .expect("a checkpoint that retained the staged batch must verify");

    let persisted = find_session_by_identifier(identifier)
        .await
        .expect("read the persisted checkpoint")
        .expect("the required checkpoint must be discoverable");
    assert_eq!(persisted.snapshot.messages.len(), 2);
}

/// Concurrent attaches for one session repair and register it exactly once.
#[tokio::test]
async fn concurrent_attach_repairs_and_registers_once() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("temporary directory for the concurrent attach test");
    // The repair checkpoint reads its archive back through the session
    // directory, so isolate it instead of scanning the host's real one.
    let session_dir = temp.path().join("sessions");
    fs::create_dir_all(&session_dir).expect("create the isolated ACP session directory");
    let _session_dir_guard = SessionDirGuard::set(&session_dir);
    let agent = build_agent(temp.path()).await;
    let workspace = AcpSessionWorkspace::from_client_cwd(temp.path())
        .await
        .expect("build the ACP session workspace from the temporary directory");
    let session_id = acp::SessionId::new("concurrent-resume-session");
    let staged_call = ToolCall::function("concurrent-call-0001".to_string(), "read_file".to_string(), "{}".to_string());

    let mut first = archived_listing("concurrent-resume-first", temp.path(), "continue");
    first.snapshot.messages = vec![SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![staged_call.clone()],
    ))];
    first.snapshot.total_messages = 1;
    let mut second = archived_listing("concurrent-resume-second", temp.path(), "continue");
    second.snapshot.messages = vec![SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![staged_call],
    ))];
    second.snapshot.total_messages = 1;

    let first_archive = first.path.clone();
    let second_archive = second.path.clone();
    let first_original = serde_json::to_vec(&first.snapshot).unwrap();
    let second_original = serde_json::to_vec(&second.snapshot).unwrap();
    fs::write(&first_archive, &first_original).unwrap();
    fs::write(&second_archive, &second_original).unwrap();

    let (first_result, second_result) = tokio::join!(
        agent.attach_thread_from_listing(&session_id, &workspace, first),
        agent.attach_thread_from_listing(&session_id, &workspace, second),
    );
    let first_handle = first_result.expect("the first concurrent attach must succeed");
    let second_handle = second_result.expect("the second concurrent attach must succeed");
    let registered = agent
        .session_handle(&session_id)
        .expect("query the session registry after concurrent attaches")
        .expect("a concurrent attach must register the session");

    assert!(
        Arc::ptr_eq(&first_handle.data, &registered.data),
        "both concurrent attaches must yield the registered session"
    );
    assert!(
        Arc::ptr_eq(&second_handle.data, &registered.data),
        "both concurrent attaches must yield the registered session"
    );
    assert_eq!(
        registered.data.lock().unwrap().thread.messages().len(),
        2,
        "the registered session must carry exactly one repaired batch"
    );

    let rewritten = [(&first_archive, &first_original), (&second_archive, &second_original)]
        .into_iter()
        .filter(|(path, original)| fs::read(path).unwrap() != **original)
        .count();
    assert_eq!(rewritten, 1, "only the winning attach may write the repair checkpoint");
}
