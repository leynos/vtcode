//! Covers approved patch execution and native ACP diff output.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

#[tokio::test]
async fn approved_apply_patch_executes_and_emits_preview_and_diff_over_acp() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let patch = "*** Begin Patch\n*** Add File: patched.txt\n+created over ACP\n*** End Patch\n";
    let patch_arguments = serde_json::json!({ "patch": patch }).to_string();
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "apply_patch",
                tool_arguments: patch_arguments.clone(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-apply-patch-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Create patched.txt"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("apply_patch protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("patched.txt")).expect("read patched file"),
        "created over ACP\n"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the tool result must return to the provider");

    let requests = requests.lock().expect("stream requests");
    let tool_response = requests[1]
        .messages
        .iter()
        .find(|message| message.is_tool_response())
        .map(|message| message.content.as_text())
        .expect("apply_patch tool response");
    assert!(tool_response.contains("\"success\":true"));
    assert!(
        !tool_response.contains("\"status\":\"incomplete\""),
        "write-ahead placeholders must be replaced before provider continuation"
    );
    drop(requests);

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    let preview_visible = updates.iter().any(|notification| {
        let acp::SessionUpdate::ToolCall(call) = &notification.update else {
            return false;
        };
        call.content.iter().any(|content| {
            matches!(
                content,
                acp::ToolCallContent::Content(block)
                    if matches!(&block.content, acp::ContentBlock::Text(text) if text.text == patch)
            )
        })
    });
    assert!(preview_visible, "the initial ACP tool call must expose the patch text");

    let diff = updates.iter().find_map(|notification| {
        let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
            return None;
        };
        update.fields.content.as_ref()?.iter().find_map(|content| match content {
            acp::ToolCallContent::Diff(diff) => Some(diff),
            _ => None,
        })
    });
    let diff = diff.expect("the completed ACP tool update must include a standardized diff");
    assert_eq!(diff.path, workspace.path().join("patched.txt"));
    assert_eq!(diff.old_text, None);
    assert_eq!(diff.new_text, "created over ACP\n");
}

/// Delete and rename patches deliver matching-hash success and native ACP diffs.
///
/// Both calls carry the version from their own source file, so the hash
/// precondition is exercised over the protocol as well as the diff rendering:
/// a delete produces a removal diff, and a rename produces one removal for the
/// source plus one creation for the destination.
#[tokio::test]
async fn approved_delete_and_rename_patches_deliver_hashed_diffs_over_acp() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let workspace = TempDir::new().expect("wire test workspace");
    std::fs::write(workspace.path().join("obsolete.txt"), "gone\n").expect("write delete fixture");
    std::fs::write(workspace.path().join("before.txt"), "moved\n").expect("write rename fixture");

    let delete_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"gone\n"));
    let delete_patch = "*** Begin Patch\n*** Delete File: obsolete.txt\n*** End Patch\n";
    let rename_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"moved\n"));
    let rename_patch =
        "*** Begin Patch\n*** Update File: before.txt\n*** Move to: after.txt\n@@\n-moved\n+relocated\n*** End Patch\n";
    let delete_arguments =
        serde_json::json!({ "input": delete_patch, "expected_content_hash": delete_hash }).to_string();
    let rename_arguments =
        serde_json::json!({ "input": rename_patch, "expected_content_hash": rename_hash }).to_string();

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "apply_patch",
                tool_arguments: delete_arguments.clone(),
                additional_tool_calls: vec![vtcode_core::llm::provider::ToolCall::function(
                    "call-rename".to_string(),
                    "apply_patch".to_string(),
                    rename_arguments.clone(),
                )],
            })
        }),
    );
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-apply-patch-diff-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Delete obsolete.txt and rename before.txt",
                    ))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("delete and rename protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert!(!workspace.path().join("obsolete.txt").exists(), "the delete must be applied");
    assert!(!workspace.path().join("before.txt").exists(), "the rename must remove its source");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("after.txt")).expect("read rename destination"),
        "relocated\n"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "both tool results must return to the provider");

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    let diffs = updates
        .iter()
        .flat_map(|notification| {
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                return Vec::new();
            };
            update
                .fields
                .content
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|content| match content {
                    acp::ToolCallContent::Diff(diff) => Some(diff),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let diff_at = |name: &str| {
        let path = workspace.path().join(name);
        diffs
            .iter()
            .find(|diff| diff.path == path)
            .unwrap_or_else(|| panic!("expected a diff for {name}: {diffs:?}"))
    };

    let deleted = diff_at("obsolete.txt");
    assert_eq!(deleted.old_text.as_deref(), Some("gone\n"));
    assert_eq!(deleted.new_text, "");

    let renamed_from = diff_at("before.txt");
    assert_eq!(renamed_from.old_text.as_deref(), Some("moved\n"));
    assert_eq!(renamed_from.new_text, "");

    let renamed_to = diff_at("after.txt");
    assert_eq!(renamed_to.old_text, None);
    assert_eq!(renamed_to.new_text, "relocated\n");
}
