//! ACP client initialization and authentication request handlers.

use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::zed::constants::INITIALIZE_VERSION_MISMATCH_LOG;
use crate::zed::helpers::agent_implementation_info;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, HandleDispatchFrom, Responder, RunWithConnectionTo, on_receive_request,
};
use std::sync::Arc;
use tracing::warn;

pub(super) fn register_client_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    let initialize_agent = Arc::clone(&agent);
    let authenticate_agent = Arc::clone(&agent);
    builder
        .on_receive_request(
            async move |req: InitializeRequest,
                        request_cx: Responder<InitializeResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_initialize(Arc::clone(&initialize_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: AuthenticateRequest,
                        request_cx: Responder<AuthenticateResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_authenticate(Arc::clone(&authenticate_agent), request_cx).await
            },
            on_receive_request!(),
        )
}

async fn handle_initialize(
    agent: Arc<ZedAgent>,
    args: InitializeRequest,
    request_cx: Responder<InitializeResponse>,
) -> Result<(), SdkError> {
    let caps = args.client_capabilities.clone();
    *agent.client_capabilities.lock().map_err(|error| {
        SdkError::internal_error().data(format!("Failed to store ACP client capabilities: {error}"))
    })? = Some(caps);
    if args.protocol_version != acp::ProtocolVersion::V1 {
        warn!(
            requested = %args.protocol_version,
            "{}",
            INITIALIZE_VERSION_MISMATCH_LOG
        );
    }
    let mut capabilities = advertised_agent_capabilities(agent.local_tool_registry.subagent_controller().is_some());
    capabilities.prompt_capabilities.embedded_context = true;
    capabilities.prompt_capabilities.image = true;
    capabilities.prompt_capabilities.audio = true;
    capabilities.mcp_capabilities.http = true;
    capabilities.mcp_capabilities.sse = false;

    let auth_methods = build_auth_methods();
    let response = InitializeResponse::new(acp::ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .agent_info(agent_implementation_info(agent.title()))
        .auth_methods(auth_methods);
    request_cx.respond(response)
}

pub(super) fn advertised_agent_capabilities(has_subagent_controller: bool) -> acp::AgentCapabilities {
    let mut capabilities = acp::AgentCapabilities::default();
    capabilities.load_session = true;
    capabilities.session_capabilities = acp::SessionCapabilities::new()
        .list(acp::SessionListCapabilities::new())
        .resume(acp::SessionResumeCapabilities::new())
        .close(acp::SessionCloseCapabilities::new());
    if has_subagent_controller {
        super::super::lody::add_lody_subagent_lifecycle_capability(&mut capabilities);
    }
    capabilities
}

fn build_auth_methods() -> Vec<acp::AuthMethod> {
    let mut methods = vec![
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openai", "OpenAI OAuth")
                .description("Authenticate with OpenAI via OAuth 2.0 with PKCE"),
        ),
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openrouter", "OpenRouter OAuth")
                .description("Authenticate with OpenRouter via OAuth 2.0 with PKCE"),
        ),
    ];
    methods.push(acp::AuthMethod::Terminal(
        acp::AuthMethodTerminal::new("terminal-login", "Terminal Login")
            .description("Interactive terminal-based authentication via vtcode login command")
            .args(vec!["login".to_string()]),
    ));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new("env-api-keys", "API Key", env_api_keys())));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new(
        "env-base-urls",
        "API Base URL",
        env_base_urls(),
    )));
    methods
}

fn env_api_keys() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str, bool)] = &[
        ("OPENAI_API_KEY", "OpenAI", false),
        ("ANTHROPIC_API_KEY", "Anthropic", false),
        ("GEMINI_API_KEY", "Google Gemini", false),
        ("OPENROUTER_API_KEY", "OpenRouter", false),
        ("DEEPSEEK_API_KEY", "DeepSeek", false),
        ("META_API_KEY", "Meta AI", false),
        ("MODEL_API_KEY", "Meta AI (documented)", false),
        ("ZAI_API_KEY", "Z.AI", false),
        ("MOONSHOT_API_KEY", "Moonshot", false),
        ("MINIMAX_API_KEY", "MiniMax", false),
        ("GROQ_API_KEY", "Groq", false),
        ("XAI_API_KEY", "xAI", false),
        ("COHERE_API_KEY", "Cohere", false),
        ("HF_TOKEN", "Hugging Face", false),
        ("MISTRAL_API_KEY", "Mistral", false),
        ("GOOGLE_API_KEY", "Google (alt)", true),
        ("OLLAMA_API_KEY", "Ollama", true),
        ("OLLAMA_API_KEY", "Ollama Cloud", false),
        ("LMSTUDIO_API_KEY", "LM Studio", true),
    ];
    ENTRIES
        .iter()
        .map(|(name, label, optional)| {
            let mut var = acp::AuthEnvVar::new(*name).label(*label);
            if *optional {
                var = var.optional(true);
            }
            var
        })
        .collect()
}

fn env_base_urls() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str)] = &[
        ("OPENAI_BASE_URL", "OpenAI"),
        ("ANTHROPIC_BASE_URL", "Anthropic"),
        ("GEMINI_BASE_URL", "Gemini"),
        ("OPENROUTER_BASE_URL", "OpenRouter"),
        ("DEEPSEEK_BASE_URL", "DeepSeek"),
        ("META_BASE_URL", "Meta AI"),
        ("ZAI_BASE_URL", "Z.AI"),
        ("MOONSHOT_BASE_URL", "Moonshot"),
        ("MINIMAX_BASE_URL", "MiniMax"),
        ("XAI_BASE_URL", "xAI"),
        ("HUGGINGFACE_BASE_URL", "Hugging Face"),
        ("OLLAMA_BASE_URL", "Ollama"),
        ("LMSTUDIO_BASE_URL", "LM Studio"),
    ];
    ENTRIES
        .iter()
        .map(|(name, label)| acp::AuthEnvVar::new(*name).label(*label).optional(true))
        .collect()
}

async fn handle_authenticate(
    _agent: Arc<ZedAgent>,
    request_cx: Responder<AuthenticateResponse>,
) -> Result<(), SdkError> {
    request_cx.respond(AuthenticateResponse::default())
}
