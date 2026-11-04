use crate::agent::runloop::unified::state::CtrlCState;
use anyhow::{Context, Result, anyhow};
use diapason_auth::{
    AuthCallbackOutcome, AuthCodeCallbackServer, AuthCredentialsStoreMode, AuthStatus, OAuthCallbackPage,
    OAuthProvider, OpenAIChatGptAuthStatus, OpenAIChatGptSession, OpenRouterToken, PkceChallenge,
    clear_oauth_token_with_mode, clear_openai_chatgpt_session, exchange_code_for_token,
    exchange_openai_chatgpt_code_for_tokens, generate_openai_oauth_state, generate_pkce_challenge,
    get_auth_status_with_mode, get_auth_url, get_openai_chatgpt_auth_status_with_mode, get_openai_chatgpt_auth_url,
    load_openai_chatgpt_session_with_mode, parse_openai_chatgpt_manual_callback_input, save_oauth_token_with_mode,
    save_openai_chatgpt_session_with_mode, start_auth_code_callback_server, try_load_codex_chatgpt_session,
};
use diapason_config::DiapasonConfig;
use diapason_core::config::api_keys::ApiKeySources;
use diapason_core::copilot::{
    COPILOT_AUTH_DOC_PATH, CopilotAuthEvent, CopilotAuthStatus, CopilotAuthStatusKind, login_with_events,
    logout_with_events, probe_auth_status,
};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::agent::runloop::unified::url_guard::{UrlGuardPrompt, open_external_url};
use crate::codex_app_server::{
    CODEX_PROVIDER, CodexAccount, CodexAccountLoginCompleted, CodexAccountReadResponse, CodexAppServerClient,
    CodexLoginAccountResponse, CodexMcpServerStatus, ServerEvent, is_codex_cli_unavailable,
};

pub(crate) const OPENAI_PROVIDER: &str = "openai";
pub(crate) const OPENROUTER_PROVIDER: &str = "openrouter";
pub(crate) const COPILOT_PROVIDER: &str = "copilot";
const DEFAULT_OPENROUTER_CALLBACK_PORT: u16 = 8484;
const DEFAULT_FLOW_TIMEOUT_SECS: u64 = diapason_config::constants::execution::DEFAULT_AUTH_FLOW_TIMEOUT_SECS;
const OPENAI_MANUAL_PLACEHOLDER: &str = "http://localhost:1455/auth/callback?code=...&state=...";

/// Prepared OpenRouter OAuth login state.
///
/// Custom `Debug` redacts the PKCE verifier (secret) and auth URL (contains
/// the challenge + state, which are short-lived secrets).
#[derive(Clone)]
pub(crate) struct PreparedOpenRouterLogin {
    pub(crate) auth_url: String,
    callback_port: u16,
    timeout_secs: u64,
    storage_mode: AuthCredentialsStoreMode,
    pkce: PkceChallenge,
}

impl std::fmt::Debug for PreparedOpenRouterLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedOpenRouterLogin")
            .field("auth_url", &"<redacted>")
            .field("callback_port", &self.callback_port)
            .field("timeout_secs", &self.timeout_secs)
            .field("storage_mode", &self.storage_mode)
            .field("pkce", &self.pkce)
            .finish()
    }
}

/// Prepared OpenAI ChatGPT OAuth login state.
///
/// Custom `Debug` redacts the PKCE verifier, auth URL, and state — all are
/// short-lived OAuth secrets that must not leak through Debug output.
#[derive(Clone)]
pub(crate) struct PreparedOpenAiLogin {
    pub(crate) auth_url: String,
    pub(crate) callback_port: u16,
    timeout_secs: u64,
    storage_mode: AuthCredentialsStoreMode,
    pkce: PkceChallenge,
    state: String,
}

impl std::fmt::Debug for PreparedOpenAiLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedOpenAiLogin")
            .field("auth_url", &"<redacted>")
            .field("callback_port", &self.callback_port)
            .field("timeout_secs", &self.timeout_secs)
            .field("storage_mode", &self.storage_mode)
            .field("pkce", &self.pkce)
            .field("state", &"<redacted>")
            .finish()
    }
}

pub(crate) struct StartedOpenRouterLogin {
    prepared: PreparedOpenRouterLogin,
    callback_server: AuthCodeCallbackServer,
}

pub(crate) struct StartedOpenAiLogin {
    prepared: PreparedOpenAiLogin,
    callback_server: AuthCodeCallbackServer,
}

pub(crate) fn supports_oauth_provider(provider: &str) -> bool {
    provider.parse::<OAuthProvider>().is_ok()
}

pub(crate) fn supports_auth_provider(provider: &str) -> bool {
    provider.eq_ignore_ascii_case(COPILOT_PROVIDER)
        || provider.eq_ignore_ascii_case(CODEX_PROVIDER)
        || supports_oauth_provider(provider)
}

pub(crate) fn prepare_openrouter_login(vt_cfg: Option<&DiapasonConfig>) -> Result<PreparedOpenRouterLogin> {
    let callback_port = vt_cfg
        .map(|cfg| cfg.auth.openrouter.callback_port)
        .unwrap_or(DEFAULT_OPENROUTER_CALLBACK_PORT);
    let timeout_secs = vt_cfg
        .map(|cfg| cfg.auth.openrouter.flow_timeout_secs)
        .unwrap_or(DEFAULT_FLOW_TIMEOUT_SECS);
    let storage_mode = credential_storage_mode(vt_cfg);
    let pkce = generate_pkce_challenge()?;
    let auth_url = get_auth_url(&pkce, callback_port);

    Ok(PreparedOpenRouterLogin {
        auth_url,
        callback_port,
        timeout_secs,
        storage_mode,
        pkce,
    })
}

pub(crate) async fn begin_openrouter_login(prepared: PreparedOpenRouterLogin) -> Result<StartedOpenRouterLogin> {
    let callback_server = start_auth_code_callback_server(
        prepared.callback_port,
        prepared.timeout_secs,
        OAuthCallbackPage::new(OAuthProvider::OpenRouter),
        None,
    )
    .await?;
    Ok(StartedOpenRouterLogin { prepared, callback_server })
}

pub(crate) async fn complete_openrouter_login(started: StartedOpenRouterLogin) -> Result<String> {
    let StartedOpenRouterLogin { prepared, callback_server } = started;

    match callback_server.wait().await? {
        AuthCallbackOutcome::Code(code) => persist_openrouter_login_code(prepared, &code).await,
        AuthCallbackOutcome::Cancelled => Err(anyhow!("OAuth flow was cancelled")),
        AuthCallbackOutcome::Error(error) => Err(anyhow!(error)),
    }
}

pub(crate) async fn complete_openrouter_login_with_tui_cancel(
    started: StartedOpenRouterLogin,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
) -> Result<String> {
    let StartedOpenRouterLogin { prepared, callback_server } = started;

    let outcome = wait_for_callback_or_cancel(callback_server.wait(), ctrl_c_state, ctrl_c_notify).await?;

    match outcome {
        AuthCallbackOutcome::Code(code) => persist_openrouter_login_code(prepared, &code).await,
        AuthCallbackOutcome::Cancelled => Err(anyhow!("OAuth flow was cancelled")),
        AuthCallbackOutcome::Error(error) => Err(anyhow!(error)),
    }
}

pub(crate) fn prepare_openai_login(vt_cfg: Option<&DiapasonConfig>) -> Result<PreparedOpenAiLogin> {
    let callback_port = vt_cfg
        .map(|cfg| cfg.auth.openai.callback_port)
        .unwrap_or(diapason_auth::OpenAIAuthConfig::default().callback_port);
    let timeout_secs = vt_cfg
        .map(|cfg| cfg.auth.openai.flow_timeout_secs)
        .unwrap_or(DEFAULT_FLOW_TIMEOUT_SECS);
    let storage_mode = credential_storage_mode(vt_cfg);
    let pkce = generate_pkce_challenge()?;
    let state = generate_openai_oauth_state()?;
    let auth_url = get_openai_chatgpt_auth_url(&pkce, callback_port, &state)?;

    Ok(PreparedOpenAiLogin {
        auth_url,
        callback_port,
        timeout_secs,
        storage_mode,
        pkce,
        state,
    })
}

pub(crate) async fn complete_openai_login(started: StartedOpenAiLogin) -> Result<OpenAIChatGptSession> {
    complete_openai_login_with_manual_future(started, None::<std::future::Ready<Result<Option<String>>>>).await
}

pub(crate) async fn begin_openai_login(prepared: PreparedOpenAiLogin) -> Result<StartedOpenAiLogin> {
    let callback_server = start_auth_code_callback_server(
        prepared.callback_port,
        prepared.timeout_secs,
        OAuthCallbackPage::new(OAuthProvider::OpenAi),
        Some(prepared.state.clone()),
    )
    .await?;
    Ok(StartedOpenAiLogin { prepared, callback_server })
}

pub(crate) async fn complete_openai_login_with_manual_future<ManualFut>(
    started: StartedOpenAiLogin,
    manual_input: Option<ManualFut>,
) -> Result<OpenAIChatGptSession>
where
    ManualFut: Future<Output = Result<Option<String>>>,
{
    let StartedOpenAiLogin { prepared, callback_server } = started;
    let code = resolve_openai_authorization_code(callback_server.wait(), &prepared.state, manual_input).await?;
    persist_openai_login_code(prepared, &code).await
}

pub(crate) async fn complete_openai_login_with_tui_cancel(
    started: StartedOpenAiLogin,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
) -> Result<OpenAIChatGptSession> {
    let StartedOpenAiLogin { prepared, callback_server } = started;

    let outcome = wait_for_callback_or_cancel(callback_server.wait(), ctrl_c_state, ctrl_c_notify).await?;

    let code = callback_outcome_to_openai_code(outcome)?;
    persist_openai_login_code(prepared, &code).await
}

pub(crate) async fn complete_openai_login_from_manual_future<ManualFut>(
    prepared: PreparedOpenAiLogin,
    manual_input: ManualFut,
) -> Result<OpenAIChatGptSession>
where
    ManualFut: Future<Output = Result<Option<String>>>,
{
    let input = manual_input
        .await?
        .ok_or_else(|| anyhow!("No redirected URL or query string provided"))?;
    let code = parse_openai_chatgpt_manual_callback_input(&input, &prepared.state)?;
    persist_openai_login_code(prepared, &code).await
}

async fn persist_openai_login_code(prepared: PreparedOpenAiLogin, code: &str) -> Result<OpenAIChatGptSession> {
    tracing::info!("received openai oauth authorization code; exchanging tokens");
    let session = exchange_openai_chatgpt_code_for_tokens(code, &prepared.pkce, prepared.callback_port).await?;
    tracing::info!("openai oauth token exchange completed; persisting session");
    save_openai_chatgpt_session_with_mode(&session, prepared.storage_mode)?;
    tracing::info!("openai oauth session persisted; verifying load");
    load_openai_chatgpt_session_with_mode(prepared.storage_mode)?
        .ok_or_else(|| anyhow!("OpenAI ChatGPT session was not persisted correctly"))
}

async fn persist_openrouter_login_code(prepared: PreparedOpenRouterLogin, code: &str) -> Result<String> {
    let api_key = exchange_code_for_token(code, &prepared.pkce).await?;
    let token = OpenRouterToken {
        api_key: api_key.clone(),
        obtained_at: now_secs(),
        expires_at: None,
        label: Some("Diapason OAuth".to_string()),
    };
    save_oauth_token_with_mode(&token, prepared.storage_mode)?;
    Ok(api_key)
}

async fn resolve_openai_authorization_code<CallbackFut, ManualFut>(
    callback_future: CallbackFut,
    expected_state: &str,
    manual_input: Option<ManualFut>,
) -> Result<String>
where
    CallbackFut: Future<Output = Result<AuthCallbackOutcome>>,
    ManualFut: Future<Output = Result<Option<String>>>,
{
    tokio::pin!(callback_future);

    let Some(manual_input) = manual_input else {
        return callback_outcome_to_openai_code(callback_future.await?);
    };

    tokio::pin!(manual_input);

    match tokio::select! {
        callback = &mut callback_future => OpenAiAuthRace::Callback(callback),
        manual = &mut manual_input => OpenAiAuthRace::Manual(manual),
    } {
        OpenAiAuthRace::Manual(Ok(Some(input))) => {
            match parse_openai_chatgpt_manual_callback_input(&input, expected_state) {
                Ok(code) => Ok(code),
                Err(manual_err) => match callback_future.await {
                    Ok(AuthCallbackOutcome::Code(code)) => Ok(code),
                    Ok(_) | Err(_) => Err(manual_err),
                },
            }
        }
        OpenAiAuthRace::Manual(Ok(None)) => callback_outcome_to_openai_code(callback_future.await?),
        OpenAiAuthRace::Manual(Err(err)) => Err(err),
        OpenAiAuthRace::Callback(Ok(outcome)) => match callback_outcome_to_openai_code(outcome) {
            Ok(code) => Ok(code),
            Err(callback_err) => match manual_input.await? {
                Some(input) => parse_openai_chatgpt_manual_callback_input(&input, expected_state),
                None => Err(callback_err),
            },
        },
        OpenAiAuthRace::Callback(Err(callback_err)) => match manual_input.await? {
            Some(input) => parse_openai_chatgpt_manual_callback_input(&input, expected_state),
            None => Err(callback_err),
        },
    }
}

async fn wait_for_callback_or_cancel<CallbackFut>(
    callback_future: CallbackFut,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
) -> Result<AuthCallbackOutcome>
where
    CallbackFut: Future<Output = Result<AuthCallbackOutcome>>,
{
    let cancel_future = wait_for_ctrl_c_cancel(ctrl_c_state, ctrl_c_notify);
    tokio::pin!(callback_future);
    tokio::pin!(cancel_future);

    tokio::select! {
        biased;
        _ = &mut cancel_future => {
            if ctrl_c_state.is_cancel_requested() {
                ctrl_c_state.mark_cancel_handled();
            }
            Ok(AuthCallbackOutcome::Cancelled)
        }
        outcome = &mut callback_future => outcome,
    }
}

async fn wait_for_ctrl_c_cancel(ctrl_c_state: &Arc<CtrlCState>, ctrl_c_notify: &Arc<Notify>) {
    if ctrl_c_state.is_cancel_requested() || ctrl_c_state.is_exit_requested() {
        return;
    }
    ctrl_c_notify.notified().await;
}

enum OpenAiAuthRace {
    Callback(Result<AuthCallbackOutcome>),
    Manual(Result<Option<String>>),
}

fn callback_outcome_to_openai_code(outcome: AuthCallbackOutcome) -> Result<String> {
    match outcome {
        AuthCallbackOutcome::Code(code) => Ok(code),
        AuthCallbackOutcome::Cancelled => Err(oauth_flow_cancelled_error()),
        AuthCallbackOutcome::Error(error) => Err(anyhow!(error)),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("OAuth flow was cancelled")]
struct OAuthFlowCancelled;

#[cold]
pub(crate) fn oauth_flow_cancelled_error() -> anyhow::Error {
    anyhow!(OAuthFlowCancelled)
}

pub(crate) fn is_oauth_flow_cancelled(err: &anyhow::Error) -> bool {
    err.is::<OAuthFlowCancelled>()
}

pub(crate) fn clear_openrouter_login(vt_cfg: Option<&DiapasonConfig>) -> Result<()> {
    clear_oauth_token_with_mode(credential_storage_mode(vt_cfg))
}

pub(crate) fn clear_openai_login(_vt_cfg: Option<&DiapasonConfig>) -> Result<()> {
    clear_openai_chatgpt_session()
}

pub(crate) fn openrouter_auth_status(vt_cfg: Option<&DiapasonConfig>) -> Result<AuthStatus> {
    get_auth_status_with_mode(credential_storage_mode(vt_cfg))
}

pub(crate) fn openai_auth_status(vt_cfg: Option<&DiapasonConfig>) -> Result<OpenAIChatGptAuthStatus> {
    get_openai_chatgpt_auth_status_with_mode(credential_storage_mode(vt_cfg))
}

pub(crate) fn load_openai_session(vt_cfg: Option<&DiapasonConfig>) -> Result<Option<OpenAIChatGptSession>> {
    load_openai_chatgpt_session_with_mode(credential_storage_mode(vt_cfg))
}

pub(crate) fn refresh_openai_login(
    vt_cfg: Option<&DiapasonConfig>,
) -> impl Future<Output = Result<OpenAIChatGptSession>> + '_ {
    diapason_auth::refresh_openai_chatgpt_session_with_mode(credential_storage_mode(vt_cfg))
}

pub(crate) async fn refresh_openai_login_if_available(
    vt_cfg: Option<&DiapasonConfig>,
) -> Result<Option<OpenAIChatGptSession>> {
    if load_openai_session(vt_cfg)?.is_none() {
        return Ok(None);
    }

    refresh_openai_login(vt_cfg).await.map(Some)
}

pub(crate) async fn handle_login_command(
    vt_cfg: Option<&DiapasonConfig>,
    provider: &str,
    device_code: bool,
    from_codex: bool,
) -> Result<()> {
    let provider = provider.trim().to_ascii_lowercase();
    if device_code && provider != CODEX_PROVIDER {
        return Err(anyhow!("`--device-code` is currently supported only for `diapason login codex`."));
    }
    if from_codex && provider != OPENAI_PROVIDER {
        return Err(anyhow!("`--from-codex` is currently supported only for `diapason login openai`."));
    }

    if provider == CODEX_PROVIDER {
        let client = CodexAppServerClient::connect(vt_cfg).await?;
        let status = client.account_read().await?;
        if status.account.is_some() {
            print_existing_codex_auth_summary(&status);
            return Ok(());
        }
        if !status.requires_openai_auth {
            println!("Codex does not currently require OpenAI authentication for this setup.");
            return Ok(());
        }

        let response = if device_code {
            client.account_login_chatgpt_device_code().await?
        } else {
            client.account_login_chatgpt().await?
        };
        let login_id = match response {
            CodexLoginAccountResponse::ChatGpt { auth_url, login_id } => {
                println!("Starting Codex ChatGPT authentication...");
                open_browser_or_print_url(&auth_url)?;
                login_id
            }
            CodexLoginAccountResponse::ChatGptDeviceCode { verification_url, user_code, login_id } => {
                println!("Starting Codex ChatGPT device-code authentication...");
                println!("Enter this code after signing in:");
                println!("{user_code}");
                diapason_commons::ansi_codes::notify_attention(true, Some(&format!("Codex device code: {user_code}")));
                open_browser_or_print_url(&verification_url)?;
                login_id
            }
            CodexLoginAccountResponse::ApiKey => {
                println!("Codex is configured with API key authentication.");
                return Ok(());
            }
            CodexLoginAccountResponse::ChatGptAuthTokens => {
                return Err(anyhow!(
                    "Codex requested externally managed ChatGPT tokens; `diapason login codex` supports only Codex-managed ChatGPT login flows"
                ));
            }
        };
        let mut events = client.subscribe();
        let completion = wait_for_codex_account_login_completion(&mut events, Some(&login_id)).await?;
        if !completion.success {
            return Err(anyhow!(completion.error.unwrap_or_else(|| "Codex ChatGPT login failed".to_string())));
        }
        let updated = client.account_read().await?;
        print_codex_login_summary(&updated);
        return Ok(());
    }

    if provider == COPILOT_PROVIDER {
        let workspace = current_auth_workspace();
        let auth_cfg = vt_cfg.map(|cfg| cfg.auth.copilot.clone()).unwrap_or_default();
        println!("Starting GitHub Copilot authentication via the official `copilot` CLI...");
        login_with_events(&auth_cfg, &workspace, print_copilot_auth_event).await?;
        println!("GitHub Copilot authentication complete.");
        return Ok(());
    }

    match provider.parse::<OAuthProvider>() {
        Ok(OAuthProvider::OpenRouter) => {
            let prepared = prepare_openrouter_login(vt_cfg)?;
            let auth_url = prepared.auth_url.clone();
            let started = begin_openrouter_login(prepared).await?;
            println!("Starting OpenRouter OAuth authentication...");
            open_browser_or_print_url(&auth_url)?;
            let api_key = complete_openrouter_login(started).await?;
            println!("OpenRouter authentication complete.");
            println!("Stored secure OAuth token. Key preview: {}...", &api_key[..api_key.len().min(8)]);
            Ok(())
        }
        Ok(OAuthProvider::OpenAi) => {
            if from_codex {
                return handle_openai_login_from_codex(vt_cfg).await;
            }
            // Inform the user if Codex fallback is already available.
            if matches!(openai_auth_status(vt_cfg)?, OpenAIChatGptAuthStatus::NotAuthenticated) {
                let overview = summarize_current_openai_credentials(vt_cfg)?;
                if overview.codex_fallback_available {
                    println!("Tip: Codex's auth.json was detected. Diapason can use it automatically at runtime.");
                    println!("Continue below for a Diapason session with full auto-refresh, or cancel and");
                    println!("run `diapason login openai --from-codex` to validate the Codex credentials.");
                }
            }
            let prepared = prepare_openai_login(vt_cfg)?;
            let started = begin_openai_login(prepared.clone()).await;
            println!("Starting OpenAI ChatGPT authentication...");
            open_browser_or_print_url(&prepared.auth_url)?;
            print_openai_manual_guidance();
            let session = complete_openai_login_with_cli_fallback(prepared, started).await?;
            println!("OpenAI ChatGPT authentication complete.");
            if let Some(email) = session.email.as_deref() {
                println!("Account: {email}");
            }
            if let Some(plan) = session.plan.as_deref() {
                println!("Plan: {plan}");
            }
            Ok(())
        }
        Err(()) => Err(anyhow!(
            "Authentication is not supported for provider '{provider}'. Supported providers: openai, openrouter, copilot, codex"
        )),
    }
}

/// Validate that Codex's `~/.codex/auth.json` contains usable ChatGPT
/// credentials and inform the user that Diapason will use them via the
/// automatic runtime fallback. Does not persist a Diapason session —
/// Codex-owned tokens are deliberately not copied or rotated by Diapason,
/// because doing so could race Codex's refresh cycle or invalidate
/// Codex-maintained credentials.
async fn handle_openai_login_from_codex(_vt_cfg: Option<&DiapasonConfig>) -> Result<()> {
    println!("Checking Codex's auth.json for ChatGPT credentials...");
    let session = try_load_codex_chatgpt_session()?.ok_or_else(|| {
        anyhow!(
            "No ChatGPT tokens found in Codex's auth.json.\n\
             Run `codex login` first to authenticate with your ChatGPT account, then retry \
             `diapason login openai --from-codex`."
        )
    })?;
    // Validate the session has a usable bearer token. The provider falls back
    // to access_token when openai_api_key is empty, so either one is sufficient.
    if session.openai_api_key.trim().is_empty() && session.access_token.trim().is_empty() {
        anyhow::bail!(
            "Codex's auth.json does not contain a usable bearer token. \
             Run `codex login` to refresh it, then retry `diapason login openai --from-codex`."
        );
    }
    println!("ChatGPT credentials found in Codex's auth.json.");
    if let Some(email) = session.email.as_deref() {
        println!("Account: {email}");
    }
    if let Some(plan) = session.plan.as_deref() {
        println!("Plan: {plan}");
    }
    println!();
    // Deliberately do NOT persist the session into Diapason's credential storage.
    // Codex-owned tokens are not copied or rotated by Diapason — doing so could
    // race Codex's refresh cycle or invalidate Codex-maintained credentials.
    // Instead, Diapason automatically falls back to Codex's auth.json at runtime
    // (with CodexAuthJsonRefresher), which re-reads the file that Codex
    // maintains independently.
    println!(
        "Diapason will use these credentials automatically at runtime via the Codex\n\
         auth.json fallback. No separate Diapason session is stored.\n\
         \n\
         Note: The tokens are managed by Codex. When they expire, run `codex login`\n\
         to refresh them. For a Diapason session with full auto-refresh support,\n\
         use `diapason login openai`."
    );
    Ok(())
}

pub(crate) async fn handle_logout_command(vt_cfg: Option<&DiapasonConfig>, provider: &str) -> Result<()> {
    let provider = provider.trim().to_ascii_lowercase();
    if provider == CODEX_PROVIDER {
        let client = CodexAppServerClient::connect(vt_cfg).await?;
        let status = client.account_read().await?;
        if status.account.is_none() {
            println!("Codex account credentials already cleared.");
            return Ok(());
        }
        client.account_logout().await?;
        println!("Codex account credentials cleared.");
        return Ok(());
    }

    if provider == COPILOT_PROVIDER {
        let workspace = current_auth_workspace();
        let auth_cfg = vt_cfg.map(|cfg| cfg.auth.copilot.clone()).unwrap_or_default();
        logout_with_events(&auth_cfg, &workspace, print_copilot_auth_event).await?;
        println!("GitHub Copilot authentication cleared.");
        return Ok(());
    }

    match provider.parse::<OAuthProvider>() {
        Ok(OAuthProvider::OpenRouter) => {
            if matches!(openrouter_auth_status(vt_cfg)?, AuthStatus::NotAuthenticated) {
                println!("OpenRouter OAuth token already cleared.");
                return Ok(());
            }
            clear_openrouter_login(vt_cfg)?;
            println!("OpenRouter OAuth token cleared.");
            Ok(())
        }
        Ok(OAuthProvider::OpenAi) => {
            let overview = summarize_current_openai_credentials(vt_cfg)?;
            if matches!(openai_auth_status(vt_cfg)?, OpenAIChatGptAuthStatus::NotAuthenticated) {
                if overview.codex_fallback_available {
                    println!("OpenAI ChatGPT session not stored; Codex auth.json fallback is active.");
                    println!("Run `codex logout` to remove the Codex credentials entirely.");
                    println!("For a Diapason session with full auto-refresh, run `diapason login openai`.");
                } else {
                    println!("OpenAI ChatGPT session already cleared.");
                }
                return Ok(());
            }
            clear_openai_login(vt_cfg)?;
            println!("OpenAI ChatGPT session cleared.");
            if overview.codex_fallback_available {
                println!("Note: Codex's auth.json was detected. Diapason will continue to use it as a");
                println!("fallback at runtime. Run `codex logout` to remove those credentials entirely.");
            }
            Ok(())
        }
        Err(()) => Err(anyhow!(
            "Authentication is not supported for provider '{provider}'. Supported providers: openai, openrouter, copilot, codex"
        )),
    }
}

pub(crate) async fn handle_show_auth_command(vt_cfg: Option<&DiapasonConfig>, provider: Option<&str>) -> Result<()> {
    println!("Authentication Status");
    println!();

    match provider.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) if value.eq_ignore_ascii_case(CODEX_PROVIDER) => {
            let client = CodexAppServerClient::connect(vt_cfg).await?;
            render_codex_auth_status(
                client.account_read().await?,
                Some(client.mcp_server_status_list().await?.data.as_slice()),
            );
        }
        Some(value) if value.eq_ignore_ascii_case(COPILOT_PROVIDER) => {
            let workspace = current_auth_workspace();
            let auth_cfg = vt_cfg.map(|cfg| cfg.auth.copilot.clone()).unwrap_or_default();
            render_copilot_auth_status(probe_auth_status(&auth_cfg, Some(&workspace)).await);
        }
        Some(value) => match value.parse::<OAuthProvider>() {
            Ok(OAuthProvider::OpenRouter) => {
                render_openrouter_auth_status(openrouter_auth_status(vt_cfg)?, credential_storage_mode(vt_cfg))
            }
            Ok(OAuthProvider::OpenAi) => {
                let overview = summarize_current_openai_credentials(vt_cfg)?;
                render_openai_auth_status(openai_auth_status(vt_cfg)?, credential_storage_mode(vt_cfg), &overview)
            }
            Err(()) => {
                return Err(anyhow!(
                    "Authentication is not supported for provider '{value}'. Supported providers: openai, openrouter, copilot, codex"
                ));
            }
        },
        None => {
            match CodexAppServerClient::connect(vt_cfg).await {
                Ok(client) => {
                    let mcp_statuses = client.mcp_server_status_list().await?;
                    render_codex_auth_status(client.account_read().await?, Some(mcp_statuses.data.as_slice()));
                }
                Err(err) if is_codex_cli_unavailable(&err) => {
                    render_codex_unavailable_status();
                }
                Err(err) => return Err(err),
            }
            println!();
            render_openrouter_auth_status(openrouter_auth_status(vt_cfg)?, credential_storage_mode(vt_cfg));
            println!();
            let openai_overview = summarize_current_openai_credentials(vt_cfg)?;
            render_openai_auth_status(openai_auth_status(vt_cfg)?, credential_storage_mode(vt_cfg), &openai_overview);
            println!();
            let workspace = current_auth_workspace();
            let auth_cfg = vt_cfg.map(|cfg| cfg.auth.copilot.clone()).unwrap_or_default();
            render_copilot_auth_status(probe_auth_status(&auth_cfg, Some(&workspace)).await);
            println!();
            println!("Use `diapason login <provider>` to authenticate.");
            println!("Use `diapason logout <provider>` to clear stored credentials.");
        }
    }

    Ok(())
}

fn credential_storage_mode(vt_cfg: Option<&DiapasonConfig>) -> AuthCredentialsStoreMode {
    vt_cfg.map(|cfg| cfg.agent.credential_storage_mode).unwrap_or_default()
}

/// Build an `OpenAICredentialOverview` from the current config and resolved
/// API key. This is the single normalized auth-source status abstraction shared
/// by CLI and TUI — both consume it instead of directly checking the filesystem.
pub(crate) fn summarize_current_openai_credentials(
    vt_cfg: Option<&DiapasonConfig>,
) -> Result<diapason_config::auth::OpenAICredentialOverview> {
    let default_auth = diapason_auth::OpenAIAuthConfig::default();
    let auth_cfg = vt_cfg.map(|cfg| &cfg.auth.openai).unwrap_or(&default_auth);
    let storage_mode = credential_storage_mode(vt_cfg);
    let api_key = diapason_core::config::api_keys::get_api_key_with_mode(
        OPENAI_PROVIDER,
        &ApiKeySources::default(),
        storage_mode,
    )
    .ok();
    diapason_config::auth::summarize_openai_credentials(auth_cfg, storage_mode, api_key)
}

fn current_auth_workspace() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn openai_manual_placeholder(callback_port: u16) -> String {
    OPENAI_MANUAL_PLACEHOLDER.replace("1455", &callback_port.to_string())
}

fn print_openai_manual_guidance() {
    println!(
        "If the browser cannot reach localhost or you are on SSH/headless, paste the redirected URL or raw query string here."
    );
}

async fn complete_openai_login_with_cli_fallback(
    prepared: PreparedOpenAiLogin,
    started: Result<StartedOpenAiLogin>,
) -> Result<OpenAIChatGptSession> {
    match started {
        Ok(started) => match complete_openai_login(started).await {
            Ok(session) => Ok(session),
            Err(err) if should_prompt_manual_openai_input(&err) => {
                let Some(input) = prompt_openai_manual_input_cli_once()? else {
                    return Err(err);
                };
                complete_openai_login_from_manual_future(prepared, std::future::ready(Ok(Some(input)))).await
            }
            Err(err) => Err(err),
        },
        Err(err) if should_prompt_manual_openai_input(&err) => {
            let Some(input) = prompt_openai_manual_input_cli_once()? else {
                return Err(err);
            };
            complete_openai_login_from_manual_future(prepared, std::future::ready(Ok(Some(input)))).await
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn should_prompt_manual_openai_input(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    message.contains("failed to bind localhost callback server") || message.contains("timed out")
}

fn prompt_openai_manual_input_cli_once() -> Result<Option<String>> {
    tokio::task::block_in_place(|| -> Result<Option<String>> {
        println!("Paste the redirected URL/query and press Enter when ready.");
        print!("Redirect URL or query: ");
        diapason_core::ui::terminal::flush_stdout();
        let input = diapason_core::ui::terminal::read_line().map_err(|err| anyhow!("stdin read failed: {err}"))?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    })
}

pub(crate) fn open_browser_or_print_url(url: &str) -> Result<()> {
    println!("Open this URL to continue:");
    println!(
        "{}{}{}",
        diapason_commons::ansi_codes::hyperlink_open(url),
        url,
        diapason_commons::ansi_codes::hyperlink_close(),
    );

    if !confirm_cli_browser_open(url)? {
        println!("Automatic browser launch skipped. Open the URL manually if you want to continue.");
        return Ok(());
    }

    if let Err(err) = open_external_url(url) {
        tracing::warn!("{err}");
    }

    Ok(())
}

async fn wait_for_codex_account_login_completion(
    events: &mut tokio::sync::broadcast::Receiver<ServerEvent>,
    login_id: Option<&str>,
) -> Result<CodexAccountLoginCompleted> {
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Err(anyhow!("lost connection while waiting for Codex login completion"));
            }
        };

        if event.method != "account/login/completed" {
            continue;
        }

        let completion: CodexAccountLoginCompleted =
            serde_json::from_value(event.params).context("invalid Codex login completion event")?;
        if login_id.is_none() || completion.login_id.as_deref() == login_id {
            return Ok(completion);
        }
    }
}

fn render_codex_auth_status(status: CodexAccountReadResponse, mcp_statuses: Option<&[CodexMcpServerStatus]>) {
    println!("Codex");
    match status.account {
        Some(CodexAccount::ApiKey) => {
            println!("  Auth: API key");
        }
        Some(CodexAccount::ChatGpt { email, plan_type }) => {
            println!("  Auth: ChatGPT");
            println!("  Account: {email}");
            println!("  Plan: {plan_type}");
        }
        None if status.requires_openai_auth => {
            println!("  Auth: not authenticated");
        }
        None => {
            println!("  Auth: not required");
        }
    }

    if let Some(mcp_statuses) = mcp_statuses
        && !mcp_statuses.is_empty()
    {
        println!("  MCP:");
        for server in mcp_statuses {
            println!("    {}: {}", server.name, server.auth_status);
        }
    }
}

fn render_codex_unavailable_status() {
    println!("Codex");
    println!("  Auth: sidecar unavailable");
    println!("  Help: {}", crate::codex_app_server::codex_sidecar_requirement_note());
}

fn print_codex_login_summary(status: &CodexAccountReadResponse) {
    println!("Codex authentication complete.");
    if let Some(CodexAccount::ChatGpt { email, plan_type }) = &status.account {
        println!("Account: {email}");
        println!("Plan: {plan_type}");
    }
}

fn print_existing_codex_auth_summary(status: &CodexAccountReadResponse) {
    match &status.account {
        Some(CodexAccount::ApiKey) => println!("Codex is already authenticated with an API key."),
        Some(CodexAccount::ChatGpt { email, plan_type }) => {
            println!("Codex is already authenticated.");
            println!("Account: {email}");
            println!("Plan: {plan_type}");
        }
        None => {}
    }
}

fn confirm_cli_browser_open(url: &str) -> Result<bool> {
    let Some(prompt) = UrlGuardPrompt::parse(url.to_string()) else {
        return Ok(false);
    };

    if diapason_core::ui::terminal::is_piped_input() || diapason_core::ui::terminal::is_piped_output() {
        println!("Automatic browser launch skipped because approval requires an interactive terminal.");
        return Ok(false);
    }

    for line in prompt.cli_lines() {
        println!("{line}");
    }

    print!("Open in browser now? [y/N]: ");
    diapason_core::ui::terminal::flush_stdout();
    let input = diapason_core::ui::terminal::read_line().map_err(|err| anyhow!("stdin read failed: {err}"))?;
    Ok(cli_browser_open_approved(&input))
}

fn cli_browser_open_approved(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn render_openrouter_auth_status(status: AuthStatus, storage_mode: AuthCredentialsStoreMode) {
    match status {
        AuthStatus::Authenticated { label, age_seconds, expires_in } => {
            println!("OpenRouter: authenticated (OAuth)");
            if let Some(label) = label {
                println!("  Label: {label}");
            }
            println!("  Token obtained: {}", format_auth_duration(age_seconds));
            if let Some(expires_in) = expires_in {
                println!("  Expires in: {}", format_auth_duration(expires_in));
            }
        }
        AuthStatus::NotAuthenticated => {
            if diapason_core::config::api_keys::get_api_key_with_mode(
                OPENROUTER_PROVIDER,
                &ApiKeySources::default(),
                storage_mode,
            )
            .is_ok()
            {
                println!("OpenRouter: using configured API key");
            } else {
                println!("OpenRouter: not authenticated");
            }
        }
    }
}

fn render_openai_auth_status(
    status: OpenAIChatGptAuthStatus,
    _storage_mode: AuthCredentialsStoreMode,
    overview: &diapason_config::auth::OpenAICredentialOverview,
) {
    use diapason_config::auth::OpenAIResolvedAuthSource;
    // Drive the headline from overview.active_source — the same field runtime
    // resolution uses — so CLI/TUI status never disagrees with actual auth.
    match overview.active_source {
        Some(OpenAIResolvedAuthSource::ChatGpt) => match status {
            OpenAIChatGptAuthStatus::Authenticated { label, age_seconds, expires_in } => {
                println!("OpenAI: authenticated (ChatGPT)");
                if let Some(label) = label {
                    println!("  Label: {label}");
                }
                println!("  Session obtained: {}", format_auth_duration(age_seconds));
                if let Some(expires_in) = expires_in {
                    println!("  Expires in: {}", format_auth_duration(expires_in));
                }
            }
            OpenAIChatGptAuthStatus::NotAuthenticated => {
                // active_source says ChatGpt but native status is NotAuthenticated
                // → this is a Codex fallback session.
                println!("OpenAI: using Codex auth.json fallback");
                if let Some(email) = overview.chatgpt_email.as_deref() {
                    println!("  Account: {email}");
                }
                if let Some(plan) = overview.chatgpt_plan.as_deref() {
                    println!("  Plan: {plan}");
                }
                println!("  Run `diapason login openai` for a Diapason session with full auto-refresh support.");
            }
        },
        Some(OpenAIResolvedAuthSource::ApiKey) => {
            println!("OpenAI: using configured API key");
        }
        None => {
            println!("OpenAI: not authenticated");
            println!("  Run `diapason login openai` to authenticate with your ChatGPT subscription.");
        }
    }
    // Show dual-credential notice when both sources are available.
    if let Some(notice) = &overview.notice {
        println!("  Note: {notice}");
    }
    if let Some(rec) = &overview.recommendation {
        println!("  {rec}");
    }
}

fn render_copilot_auth_status(status: CopilotAuthStatus) {
    match status.kind {
        CopilotAuthStatusKind::Authenticated => {
            println!("GitHub Copilot: authenticated (managed auth via Copilot CLI)")
        }
        CopilotAuthStatusKind::Unauthenticated => println!("GitHub Copilot: not authenticated"),
        CopilotAuthStatusKind::ServerUnavailable => println!("GitHub Copilot: CLI unavailable"),
        CopilotAuthStatusKind::AuthFlowFailed => println!("GitHub Copilot: auth flow failed"),
    }

    if let Some(message) = status.message.as_deref()
        && !message.trim().is_empty()
    {
        println!("  Details: {message}");
    }

    if matches!(status.kind, CopilotAuthStatusKind::ServerUnavailable) {
        println!("  Help: install `copilot` or configure `[auth.copilot].command`; see {COPILOT_AUTH_DOC_PATH}");
    }
}

fn print_copilot_auth_event(event: CopilotAuthEvent) -> Result<()> {
    match event {
        CopilotAuthEvent::VerificationCode { url, user_code } => {
            println!("GitHub device code (copy this): {user_code}");
            diapason_commons::ansi_codes::notify_attention(true, Some(&format!("GitHub device code: {user_code}")));
            println!("Open this URL to continue:");
            println!("{url}");
        }
        CopilotAuthEvent::Progress { message } => println!("{message}"),
        CopilotAuthEvent::Success { account } => {
            if let Some(account) = account {
                println!("GitHub Copilot account: {account}");
            }
        }
        CopilotAuthEvent::Failure { message } => {
            tracing::error!("GitHub Copilot auth failed: {message}")
        }
    }

    Ok(())
}

fn format_auth_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else if seconds < 86_400 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3600)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        clear_openai_login, clear_openrouter_login, cli_browser_open_approved, handle_login_command,
        resolve_openai_authorization_code, wait_for_callback_or_cancel,
    };
    use crate::agent::runloop::unified::state::{CtrlCSignal, CtrlCState};
    use anyhow::anyhow;
    use diapason_auth::{
        AuthCallbackOutcome, AuthCredentialsStoreMode, OpenAIAuthConfig, OpenAIChatGptSession, OpenAIResolvedAuth,
        OpenRouterToken, clear_oauth_token_with_mode, clear_openai_chatgpt_session,
        clear_openai_chatgpt_session_with_mode, load_oauth_token_with_mode, load_openai_chatgpt_session_with_mode,
        resolve_openai_auth, save_oauth_token_with_mode, save_openai_chatgpt_session_with_mode,
    };
    use serial_test::serial;
    use std::future;
    use std::sync::Arc;
    use tokio::sync::Notify;

    /// Guard that isolates `CODEX_HOME` to an empty temp dir for the duration
    /// of a test, preventing the Codex auth.json fallback from picking up a
    /// real Codex session from the user's machine.
    struct CodexHomeGuard {
        _temp: tempfile::TempDir,
        previous: Option<String>,
    }

    impl CodexHomeGuard {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("create temp codex home");
            let previous = std::env::var("CODEX_HOME").ok();
            diapason_commons::env_lock::set_var("CODEX_HOME", temp.path());
            Self { _temp: temp, previous }
        }
    }

    impl Drop for CodexHomeGuard {
        fn drop(&mut self) {
            diapason_commons::env_lock::lock().restore_var("CODEX_HOME", self.previous.as_deref());
        }
    }
    use diapason_config::DiapasonConfig;

    fn config_with_storage_mode(mode: AuthCredentialsStoreMode) -> DiapasonConfig {
        let mut config = DiapasonConfig::default();
        config.agent.credential_storage_mode = mode;
        config
    }

    fn sample_openai_session(api_key: &str) -> OpenAIChatGptSession {
        OpenAIChatGptSession {
            openai_api_key: api_key.to_string(),
            id_token: "id-token".to_string(),
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            account_id: Some("account-123".to_string()),
            email: Some("user@example.com".to_string()),
            plan: Some("plus".to_string()),
            obtained_at: 1,
            refreshed_at: 1,
            expires_at: Some(2),
        }
    }

    fn sample_openrouter_token(api_key: &str) -> OpenRouterToken {
        OpenRouterToken {
            api_key: api_key.to_string(),
            obtained_at: 1,
            expires_at: None,
            label: Some("test-token".to_string()),
        }
    }

    #[test]
    fn cli_browser_open_approved_accepts_yes_only() {
        assert!(cli_browser_open_approved("y"));
        assert!(cli_browser_open_approved("YES"));
        assert!(!cli_browser_open_approved("n"));
        assert!(!cli_browser_open_approved(""));
    }

    #[tokio::test]
    async fn openai_code_resolution_accepts_manual_query_input() {
        let code = resolve_openai_authorization_code(
            future::pending::<Result<AuthCallbackOutcome, anyhow::Error>>(),
            "test-state",
            Some(future::ready(Ok(Some("code=manual-code&state=test-state".to_string())))),
        )
        .await
        .expect("manual input should resolve");
        assert_eq!(code, "manual-code");
    }

    #[tokio::test]
    async fn openai_code_resolution_falls_back_to_manual_when_callback_fails() {
        let code = resolve_openai_authorization_code(
            future::ready(Err(anyhow!("failed to bind localhost callback server"))),
            "test-state",
            Some(future::ready(Ok(Some(
                "http://localhost:1455/auth/callback?code=manual-code&state=test-state".to_string(),
            )))),
        )
        .await
        .expect("manual fallback should resolve");
        assert_eq!(code, "manual-code");
    }

    #[tokio::test]
    async fn openai_code_resolution_rejects_manual_bare_code() {
        let error = resolve_openai_authorization_code(
            future::ready(Err(anyhow!("OAuth flow timed out after 300 seconds"))),
            "test-state",
            Some(future::ready(Ok(Some("manual-code".to_string())))),
        )
        .await
        .expect_err("bare code should fail");
        assert!(error.to_string().contains("full redirect url or query string"));
    }

    #[tokio::test]
    async fn openai_code_resolution_uses_callback_when_manual_input_is_invalid() {
        let code = resolve_openai_authorization_code(
            future::ready(Ok(AuthCallbackOutcome::Code("callback-code".to_string()))),
            "test-state",
            Some(future::ready(Ok(Some("manual-code".to_string())))),
        )
        .await
        .expect("callback code should win after invalid manual input");
        assert_eq!(code, "callback-code");
    }

    #[tokio::test]
    async fn callback_wait_returns_cancelled_when_ctrl_c_is_pending() {
        let ctrl_c_state = Arc::new(CtrlCState::new());
        assert!(matches!(ctrl_c_state.register_signal(), CtrlCSignal::Cancel));
        let ctrl_c_notify = Arc::new(Notify::new());

        let outcome = wait_for_callback_or_cancel(
            future::pending::<Result<AuthCallbackOutcome, anyhow::Error>>(),
            &ctrl_c_state,
            &ctrl_c_notify,
        )
        .await
        .expect("cancelled callback wait");

        assert!(matches!(outcome, AuthCallbackOutcome::Cancelled));
        assert!(!ctrl_c_state.is_cancel_requested());
    }

    #[tokio::test]
    async fn device_code_flag_is_rejected_for_non_codex_provider() {
        let error = handle_login_command(None, "openai", true, false)
            .await
            .expect_err("non-codex device-code login should fail");

        assert!(error.to_string().contains("supported only for `diapason login codex`"));
    }

    #[tokio::test]
    async fn from_codex_flag_is_rejected_for_non_openai_provider() {
        let error = handle_login_command(None, "openrouter", false, true)
            .await
            .expect_err("non-openai from-codex login should fail");

        assert!(error.to_string().contains("supported only for `diapason login openai`"));
    }

    #[test]
    #[serial]
    fn openai_logout_clears_all_sessions_and_falls_back_to_api_key() {
        let _codex_guard = CodexHomeGuard::new();
        let _ = clear_openai_chatgpt_session();
        let _ = clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File);
        let _ = clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::Keyring);

        let file_session = sample_openai_session("file-api-key");
        let keyring_session = sample_openai_session("keyring-api-key");
        if save_openai_chatgpt_session_with_mode(&file_session, AuthCredentialsStoreMode::File).is_err() {
            return;
        }

        if save_openai_chatgpt_session_with_mode(&keyring_session, AuthCredentialsStoreMode::Keyring).is_err() {
            let _ = clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File);
            return;
        }

        let config = config_with_storage_mode(AuthCredentialsStoreMode::File);
        clear_openai_login(Some(&config)).expect("clear openai login");

        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load file session")
                .is_none()
        );
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::Keyring)
                .expect("load keyring session")
                .is_none()
        );
        assert!(matches!(
            resolve_openai_auth(
                &OpenAIAuthConfig::default(),
                AuthCredentialsStoreMode::File,
                Some("env-api-key".to_string())
            )
            .expect("resolve auth after logout"),
            OpenAIResolvedAuth::ApiKey { api_key } if api_key == "env-api-key"
        ));

        let _ = clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File);
        let _ = clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::Keyring);
    }

    #[test]
    #[serial]
    fn openrouter_logout_clears_only_configured_storage_mode() {
        let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::File);
        let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::Keyring);

        let file_token = sample_openrouter_token("file-token");
        let keyring_token = sample_openrouter_token("keyring-token");
        if save_oauth_token_with_mode(&file_token, AuthCredentialsStoreMode::File).is_err() {
            return;
        }

        if save_oauth_token_with_mode(&keyring_token, AuthCredentialsStoreMode::Keyring).is_err() {
            let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::File);
            return;
        }

        let Some(loaded_keyring_token) =
            load_oauth_token_with_mode(AuthCredentialsStoreMode::Keyring).expect("load keyring token after save")
        else {
            let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::File);
            let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::Keyring);
            return;
        };
        if loaded_keyring_token.api_key != "keyring-token" {
            let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::File);
            let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::Keyring);
            return;
        }

        let config = config_with_storage_mode(AuthCredentialsStoreMode::File);
        clear_openrouter_login(Some(&config)).expect("clear openrouter login");

        assert!(
            load_oauth_token_with_mode(AuthCredentialsStoreMode::File)
                .expect("load file token")
                .is_none()
        );
        assert_eq!(
            load_oauth_token_with_mode(AuthCredentialsStoreMode::Keyring)
                .expect("load keyring token")
                .expect("keyring token should remain")
                .api_key,
            "keyring-token"
        );

        let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::File);
        let _ = clear_oauth_token_with_mode(AuthCredentialsStoreMode::Keyring);
    }
}
