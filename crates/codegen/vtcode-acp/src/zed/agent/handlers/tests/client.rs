//! Checks the ACP initialize and authenticate handler registration contract.
//!
//! These tests exercise the repository-owned registration and projection
//! boundaries in `handlers/client.rs`: the handlers must be reachable through
//! the shared builder and `handle_initialize` must advertise the capabilities
//! and authentication methods that clients rely on. They deliberately do not
//! exercise OAuth providers or SDK serialization internals.
use super::*;
use agent_client_protocol::schema::v1::{AuthMethod, AuthenticateRequest, InitializeRequest};
use std::time::Duration;

/// Install the agent handlers and run `exercise` against a duplex client.
async fn with_initialized_client<F, Fut>(mut exercise: F)
where
    F: FnMut(ConnectionTo<Agent>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let workspace = TempDir::new().expect("temporary ACP workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection =
        install_handlers(Agent.builder().name("vtcode-init-test"), Arc::clone(&agent)).connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            exercise(cx).await;
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("initialization and authentication request should be served")
        .expect("client connection should complete");
    agent_task.abort();
}

/// Collect the advertised authentication methods keyed by their identifier.
fn methods_by_id(methods: &[AuthMethod]) -> BTreeMap<String, &AuthMethod> {
    methods.iter().map(|method| (method.id().0.to_string(), method)).collect()
}

#[tokio::test]
async fn initialize_advertises_repository_capabilities_and_auth_methods() {
    with_initialized_client(|cx| async move {
        let response = cx
            .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await
            .expect("initialize request should succeed");

        assert_eq!(response.protocol_version, acp::ProtocolVersion::V1);

        let capabilities = &response.agent_capabilities;
        assert!(capabilities.load_session);
        assert!(capabilities.session_capabilities.list.is_some());
        assert!(capabilities.session_capabilities.resume.is_some());
        assert!(capabilities.session_capabilities.close.is_some());
        assert!(capabilities.prompt_capabilities.embedded_context);
        assert!(capabilities.prompt_capabilities.image);
        assert!(capabilities.prompt_capabilities.audio);
        assert!(capabilities.mcp_capabilities.http);
        assert!(!capabilities.mcp_capabilities.sse);

        let methods = methods_by_id(&response.auth_methods);
        for expected in [
            "oauth-openai",
            "oauth-openrouter",
            "terminal-login",
            "env-api-keys",
            "env-base-urls",
        ] {
            assert!(methods.contains_key(expected), "initialize must advertise the {expected} authentication method");
        }
    })
    .await;
}

#[tokio::test]
async fn terminal_login_auth_method_includes_the_login_argument() {
    with_initialized_client(|cx| async move {
        let response = cx
            .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await
            .expect("initialize request should succeed");
        let methods = methods_by_id(&response.auth_methods);

        let Some(AuthMethod::Terminal(terminal)) = methods.get("terminal-login") else {
            panic!("terminal-login must be advertised as a terminal auth method");
        };
        assert_eq!(terminal.args, vec!["login".to_string()]);
    })
    .await;
}

#[tokio::test]
async fn api_key_auth_method_preserves_declared_optional_flags() {
    with_initialized_client(|cx| async move {
        let response = cx
            .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await
            .expect("initialize request should succeed");
        let methods = methods_by_id(&response.auth_methods);

        let Some(AuthMethod::EnvVar(keys)) = methods.get("env-api-keys") else {
            panic!("env-api-keys must be advertised as an environment-variable auth method");
        };

        // `OLLAMA_API_KEY` appears twice with different labels, so key by label.
        let by_label: BTreeMap<&str, &acp::AuthEnvVar> = keys
            .vars
            .iter()
            .map(|var| (var.label.as_deref().expect("every API-key entry has a label"), var))
            .collect();

        let required = ["OpenAI", "Anthropic", "OpenRouter", "Groq"];
        for label in required {
            let var = by_label
                .get(label)
                .unwrap_or_else(|| panic!("{label} API-key entry must be advertised"));
            assert!(!var.optional, "{label} API key must be required");
        }

        let optional = ["Google (alt)", "Ollama", "LM Studio"];
        for label in optional {
            let var = by_label
                .get(label)
                .unwrap_or_else(|| panic!("{label} API-key entry must be advertised"));
            assert!(var.optional, "{label} API key must preserve its optional flag");
        }

        let ollama_cloud = by_label
            .get("Ollama Cloud")
            .expect("Ollama Cloud API-key entry must be advertised");
        assert!(!ollama_cloud.optional, "Ollama Cloud API key must be required");
    })
    .await;
}

#[tokio::test]
async fn base_url_auth_method_entries_are_all_optional() {
    with_initialized_client(|cx| async move {
        let response = cx
            .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await
            .expect("initialize request should succeed");
        let methods = methods_by_id(&response.auth_methods);

        let Some(AuthMethod::EnvVar(urls)) = methods.get("env-base-urls") else {
            panic!("env-base-urls must be advertised as an environment-variable auth method");
        };
        assert!(!urls.vars.is_empty(), "base-URL entries must be advertised");
        for var in &urls.vars {
            assert!(var.optional, "{} must remain an optional base URL", var.name);
        }
    })
    .await;
}

#[tokio::test]
async fn authenticate_request_is_dispatched_and_returns_an_empty_response() {
    with_initialized_client(|cx| async move {
        let _initialize_response = cx
            .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await
            .expect("initialize request should succeed");

        // Dispatching the request proves the handler is registered. The empty
        // response is the documented contract: no OAuth flow runs and no
        // credentials are validated here.
        let response = cx
            .send_request(AuthenticateRequest::new("terminal-login"))
            .block_task()
            .await
            .expect("authenticate request should be dispatched");
        assert!(response.meta.is_none());
    })
    .await;
}
