//! Proves the write-ahead checkpoint is durable before tool execution starts.
//!
//! The tool command itself inspects the durable archive and only then blocks on
//! a FIFO, so a command that reached the FIFO is evidence that the staged call
//! and its placeholder were already persisted when execution began.
use super::*;
use crate::zed::agent::handlers::prompt::{
    SystemToolRoundTimer, ToolRoundOutcome, ToolRoundRequest, execute_tool_round,
};
use agent_client_protocol::schema::v1::{NewSessionRequest, SetSessionConfigOptionRequest};
use tokio::sync::oneshot;
/// Proves the staged call and placeholder are durable when execution begins.
///
/// The tool command runs under `sh -c` and chains three read-only steps: it
/// greps the durable session archive for the staged call, greps it again for
/// the incomplete placeholder, and only then blocks reading a FIFO. Shell
/// `&&` short-circuits, so the command can reach the FIFO only if both records
/// were already persisted when the tool process started. The test asserts the
/// round is still running once the archive is externally visible, which means
/// the command is parked inside `tail` and the in-path proof has succeeded.
/// Moving staging after execution would make a grep fail, finish the round
/// early, and fail the test.
#[tokio::test]
async fn staged_call_and_placeholder_are_durable_before_tool_execution_completes() {
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("wire test workspace");
    let gate_path = workspace.path().join("release-gate");
    create_release_gate(&gate_path);
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create ACP session")
        .session_id;
    // The default duck agent denies exec_command; the build agent admits it.
    let _ = agent
        .set_session_config_option(SetSessionConfigOptionRequest::new(session_id.clone(), "primary_agent", "build"))
        .await
        .expect("select the build primary agent");
    let session = agent
        .session_handle(&session_id)
        .expect("ACP session lookup")
        .expect("created ACP session");

    // Attach a live client so tool execution can proceed. The client only
    // needs to keep the connection open; it asserts nothing in this test.
    let (agent_channel, client_channel) = Channel::duplex();
    let (cx_tx, cx_rx) = oneshot::channel();
    let agent_task =
        tokio::spawn(
            install_handlers(Agent.builder().name("diapason-execution-boundary-test"), Arc::clone(&agent))
                .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                    drop(cx_tx.send(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }),
        );
    let cx = cx_rx.await.expect("capture the agent connection handle");
    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));

    let call_id = "call-execution-boundary";
    // The two greps are the in-path ordering proof: `&&` reaches `tail` only
    // when the durable archive already holds the staged call and its
    // incomplete placeholder. `tail` then blocks opening the gate until the
    // test opens it for writing. Every segment is a read-only command, and the
    // build agent admits `exec_command`, so the policy allows the chain.
    let session_root = session_dir.path().display();
    let command = format!(
        "grep -rq '{call_id}' '{session_root}' && grep -rq 'durable terminal result' '{session_root}' && tail '{}'",
        gate_path.display()
    );
    let calls = vec![diapason_core::llm::provider::ToolCall::function(
        call_id.to_string(),
        "exec_command".to_string(),
        serde_json::json!({ "cmd": command }).to_string(),
    )];
    let mut tool_loop_count = 0usize;
    let mut round = tokio::spawn({
        let agent = Arc::clone(&agent);
        let session = session.clone();
        let session_id = session_id.clone();
        async move {
            execute_tool_round(ToolRoundRequest {
                agent: agent.as_ref(),
                session: &session,
                session_id: &session_id,
                tool_calls: &calls,
                assistant_message: "Run the blocking command.",
                assistant_reasoning: "",
                reasoning_details: None,
                tool_loop_count: &mut tool_loop_count,
                streamed: false,
                timer: &SystemToolRoundTimer,
            })
            .await
        }
    });

    // Poll the durable archive until the assistant call and its incomplete
    // placeholder are both present. The command is still blocked on the gate,
    // so this window is strictly before execution completes.
    let current_archive_path = || {
        session
            .data
            .lock()
            .ok()
            .and_then(|data| data.archive.as_ref().map(|archive| archive.path().to_path_buf()))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let archive_path = loop {
        if let Some(path) = current_archive_path()
            && read_archive(&path).is_some_and(|snapshot| archive_has_staged_call(&snapshot, call_id))
        {
            break path;
        }
        // A round that already finished cannot become durable later, so report
        // its outcome instead of waiting out the deadline.
        if round.is_finished() {
            let outcome = (&mut round).await.expect("tool round task should not panic");
            panic!(
                "the tool round finished before the staged call became durable: {}; archive={}",
                describe_round_result(&outcome),
                describe_archive_at(current_archive_path().as_deref())
            );
        }
        assert!(
            Instant::now() < deadline,
            "the staged call and its placeholder must be durable before execution completes; archive={}",
            describe_archive_at(current_archive_path().as_deref())
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(!round.is_finished(), "the tool command must still be blocked at the execution boundary");
    let snapshot = read_archive(&archive_path).expect("read durable archive");
    assert!(
        archive_has_staged_call(&snapshot, call_id),
        "the durable archive must hold the assistant call and its incomplete placeholder"
    );

    release_gate(&gate_path).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), round)
        .await
        .expect("the released tool round should finish")
        .expect("tool round task should not panic")
        .expect("the released tool round should succeed");
    assert!(matches!(outcome, ToolRoundOutcome::Continue), "the released round should continue the tool loop");
    assert!(
        session.data.lock().expect("session data lock").write_ahead_call_ids.is_empty(),
        "terminal results released by a checkpoint that retains them must clear write-ahead protection"
    );

    agent_task.abort();
    drop(agent_task.await);
    drop(client_channel);
}

/// Creates the FIFO that holds the tool command at the execution boundary.
///
/// A FIFO is the only gate the execution policy admits: the allow-list has no
/// shell, so a polling loop cannot be used, and `tail` blocks opening a FIFO
/// until a writer appears.
fn create_release_gate(path: &std::path::Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo should be available to create the release gate");
    assert!(status.success(), "mkfifo must create the release gate at {}", path.display());
}

/// Opens the release gate so the blocked command observes end-of-file.
///
/// Opening for writing rendezvous with the blocked reader, so the open runs on
/// a blocking thread and is bounded in case the command never starts.
async fn release_gate(path: &std::path::Path) {
    let path = path.to_path_buf();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || std::fs::write(&path, b"release")),
    )
    .await
    .expect("the release writer should rendezvous with the blocked command")
    .expect("release writer task should not panic")
    .expect("release the blocked command");
}

/// Describes a finished tool round for a failure message.
fn describe_round_result<E: std::fmt::Debug>(result: &Result<ToolRoundOutcome, E>) -> String {
    match result {
        Ok(ToolRoundOutcome::Continue) => "completed with Continue".to_string(),
        Ok(ToolRoundOutcome::Cancelled) => "completed with Cancelled".to_string(),
        Ok(ToolRoundOutcome::LimitReached(message)) => format!("completed with LimitReached({message})"),
        Err(error) => format!("failed with {error:?}"),
    }
}

/// Describes the session archive for a failure message.
///
/// The contents are bounded so a large archive cannot flood the test log.
fn describe_archive_at(path: Option<&std::path::Path>) -> String {
    let Some(path) = path else {
        return "no archive attached".to_string();
    };
    match std::fs::read_to_string(path) {
        Ok(contents) => format!("{} ({})", path.display(), describe_archive_contents(&contents)),
        Err(error) => format!("{} (unreadable: {error})", path.display()),
    }
}

/// Renders at most 2000 characters of an archive document for a failure message.
fn describe_archive_contents(contents: &str) -> String {
    const LIMIT: usize = 2000;
    let Some((offset, _)) = contents.char_indices().nth(LIMIT) else {
        return contents.to_string();
    };
    // `offset` comes from `char_indices`, so it is a character boundary; `get`
    // keeps the truncation UTF-8 safe without indexing into the string.
    let prefix = contents.get(..offset).unwrap_or(contents);
    format!("{prefix}…")
}

/// Parses a session archive snapshot, returning `None` while it is absent or
/// not yet a complete JSON document.
fn read_archive(path: &std::path::Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether the snapshot holds both the assistant tool call and its incomplete
/// placeholder for `call_id`.
fn archive_has_staged_call(snapshot: &serde_json::Value, call_id: &str) -> bool {
    let Some(messages) = snapshot.get("messages").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let has_assistant_call = messages.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("Assistant")
            && message
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|calls| {
                    calls
                        .iter()
                        .any(|call| call.get("id").and_then(serde_json::Value::as_str) == Some(call_id))
                })
    });
    let has_placeholder = messages.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("Tool")
            && message.get("tool_call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            && message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.contains("\"status\":\"incomplete\""))
    });
    has_assistant_call && has_placeholder
}
