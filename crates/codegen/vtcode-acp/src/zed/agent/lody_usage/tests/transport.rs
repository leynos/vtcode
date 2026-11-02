//! Wire-level test for the Lody usage extension notification.

use super::*;

/// Assert usage travels over the official ACP extension channel.
#[tokio::test]
async fn sends_usage_through_the_official_acp_extension_channel() {
    let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
    let (received_tx, mut received_rx) = mpsc::unbounded_channel();
    let client_ready = Arc::new(Notify::new());
    let agent_ready = Arc::clone(&client_ready);
    let notification_received = Arc::new(Notify::new());
    let agent_ack = Arc::clone(&notification_received);

    let agent_connection = Agent.builder().name("vtcode-usage-test").connect_with(
        agent_channel,
        move |cx: ConnectionTo<Client>| async move {
            agent_ready.notified().await;
            let client = ConnectionHandle::new(cx);
            send_usage_update(
                &client,
                &acp::SessionId::new(Arc::from("session-wire")),
                "wire-model",
                &Usage {
                    prompt_tokens: 13,
                    completion_tokens: 5,
                    ..Usage::default()
                },
                UsageProfile {
                    context_window: Some(524_288),
                    pricing: None,
                    exclusive_input: false,
                },
            )
            .expect("send usage update");
            agent_ack.notified().await;
            Ok(())
        },
    );
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                drop(received_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |_cx: ConnectionTo<Agent>| {
            client_ready.notify_one();
            let notification = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("usage notification deadline")
                .expect("usage notification");
            let acp::AgentNotification::ExtNotification(notification) = notification else {
                panic!("usage must use an ACP extension notification");
            };
            assert_eq!(
                notification.method.as_ref().trim_start_matches('_'),
                LODY_SESSION_USAGE_UPDATE_METHOD.trim_start_matches('_')
            );
            let value: serde_json::Value =
                serde_json::from_str(notification.params.get()).expect("usage notification JSON");
            assert_eq!(value["sessionId"], "session-wire");
            assert_eq!(value["usage"]["inputTokens"], 13);
            assert_eq!(value["usage"]["outputTokens"], 5);
            assert_eq!(value["usage"]["contextWindow"], 524_288);
            notification_received.notify_one();
            Ok(())
        });

    tokio::time::timeout(std::time::Duration::from_secs(3), client_connection)
        .await
        .expect("client connection deadline")
        .expect("client connection");
    agent_task.await.expect("agent task").expect("agent connection");
}
