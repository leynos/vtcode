//! OpenAI ChatGPT subscription OAuth flow and secure session storage.
//!
//! This module implements an OAuth 2.0 PKCE authorization-code flow for ChatGPT
//! subscription auth, mirroring the flow used by [openai/codex]. By default VT
//! Code reuses the Codex CLI's **public PKCE OAuth client identity** (no client
//! secret — the ID is not a secret by OAuth 2.1 design). This is an **unofficial
//! compatibility mechanism**: OpenAI has not documented or guaranteed third-party
//! reuse of this client identity, and a public client ID is not authorization
//! to reuse another tool's OAuth registration. This allows ChatGPT subscription
//! login to work without the Codex CLI installed.
//! Organizations with their own OpenAI-issued client can override via
//! `VTCODE_OPENAI_OAUTH_CLIENT_ID` / `VTCODE_OPENAI_OAUTH_ORIGINATOR`.
//!
//! - OAuth authorization-code flow with PKCE
//! - refresh-token exchange
//! - token exchange for an OpenAI API-key-style bearer token
//! - secure storage in keyring or encrypted file storage
//!
//! Based on patterns from [openai/codex] (Apache-2.0). Copyright 2025 OpenAI.
//! See the repository `THIRD-PARTY-NOTICES` file for full attribution.
//!
//! [openai/codex]: https://github.com/openai/codex

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use reqwest::Client;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

use crate::storage_paths::auth_storage_dir;
use crate::{OpenAIAuthConfig, OpenAIPreferredMethod};

pub use super::credentials::AuthCredentialsStoreMode;
use super::pkce::PkceChallenge;
#[cfg(test)]
use crate::openai_refresh_policy::extract_error_code;
use crate::openai_refresh_policy::{RefreshFailureAction, classify_refresh_failure};
use crate::openai_session_storage::OpenAiSessionStorage;
#[cfg(test)]
use crate::openai_session_storage::{
    decrypt_legacy_session as decrypt_session, encrypt_legacy_session as encrypt_session,
    legacy_session_path as get_session_path,
};

const OPENAI_AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Default OAuth client identity.
///
/// This is the **Codex CLI's public PKCE OAuth client ID**. VT Code reuses
/// Codex's public client identity (a PKCE public client with no client secret
/// — the ID is not a secret by OAuth 2.1 design) as an **unofficial
/// compatibility mechanism**. OpenAI has not documented or guaranteed
/// third-party reuse of this identity, and a public client ID is not
/// authorization to reuse another tool's OAuth registration. This lets VT
/// Code perform ChatGPT subscription login without requiring the Codex CLI
/// to be installed.
///
/// Organizations with their own OpenAI-issued OAuth client can override this
/// via the `VTCODE_OPENAI_OAUTH_CLIENT_ID` environment variable.
///
/// See `docs/guides/oauth-authentication.md` for the full explanation.
const DEFAULT_OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Default originator sent to OpenAI's authorization endpoint.
///
/// This matches the Codex CLI's originator because the default client ID is
/// Codex's. Override with `VTCODE_OPENAI_OAUTH_ORIGINATOR` when using a custom
/// client ID.
const DEFAULT_OPENAI_ORIGINATOR: &str = "codex_cli_rs";
/// Maximum bytes read from a token-endpoint error response body for
/// classification. Prevents unbounded reads from a misbehaving or hostile
/// endpoint while still capturing standard OAuth 2.0 error JSON.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
const OPENAI_CALLBACK_PATH: &str = "/auth/callback";
const OPENAI_REFRESH_LOCK_FILE: &str = "openai_chatgpt.refresh.lock";
const REFRESH_INTERVAL_SECS: u64 = 8 * 60;
const REFRESH_SKEW_SECS: u64 = 60;

/// Resolved OAuth client identity (client ID + originator).
///
/// Both fields must be consistent: when a custom client ID is provided via
/// `VTCODE_OPENAI_OAUTH_CLIENT_ID`, the originator must also be overridden
/// via `VTCODE_OPENAI_OAUTH_ORIGINATOR`. Sending a custom client ID with
/// Codex's `codex_cli_rs` originator (or vice versa) would be inconsistent
/// and is rejected.
///
/// `Debug` is safe to derive: the client ID is a public PKCE client
/// identifier (not a secret by OAuth 2.1 design), and the originator is
/// a public identifier string.
#[derive(Debug)]
struct OAuthClientIdentity {
    client_id: String,
    originator: String,
}

/// Resolve the OAuth client identity from environment variables.
///
/// ## Invariant
///
/// The client ID and originator form a **coherent pair**. One-sided overrides
/// are rejected to prevent mixed identities (e.g. a custom client ID paired
/// with Codex's `codex_cli_rs` originator).
///
/// - Both `VTCODE_OPENAI_OAUTH_CLIENT_ID` and `VTCODE_OPENAI_OAUTH_ORIGINATOR`
///   set and non-blank → use the custom pair.
/// - Neither set → use the complete Codex default pair.
/// - Only one set → return a configuration error with an actionable message.
///   The caller must surface this so the user can fix the environment before
///   any OAuth request is sent.
///
/// All four flow stages (authorization URL, code exchange, refresh, token
/// exchange) call this resolver, so the same coherent pair is used throughout.
fn resolve_oauth_client_identity() -> Result<OAuthClientIdentity> {
    let custom_client_id = std::env::var("VTCODE_OPENAI_OAUTH_CLIENT_ID")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let custom_originator = std::env::var("VTCODE_OPENAI_OAUTH_ORIGINATOR")
        .ok()
        .filter(|v| !v.trim().is_empty());

    match (custom_client_id, custom_originator) {
        (Some(id), Some(originator)) => Ok(OAuthClientIdentity { client_id: id, originator }),
        (Some(_), None) => bail!(
            "VTCODE_OPENAI_OAUTH_CLIENT_ID is set but VTCODE_OPENAI_OAUTH_ORIGINATOR is not. \
             The client ID and originator must be overridden together to form a coherent OAuth \
             identity. Set VTCODE_OPENAI_OAUTH_ORIGINATOR to match your custom client ID, \
             or unset VTCODE_OPENAI_OAUTH_CLIENT_ID to use the default Codex identity."
        ),
        (None, Some(_)) => bail!(
            "VTCODE_OPENAI_OAUTH_ORIGINATOR is set but VTCODE_OPENAI_OAUTH_CLIENT_ID is not. \
             The client ID and originator must be overridden together to form a coherent OAuth \
             identity. Set VTCODE_OPENAI_OAUTH_CLIENT_ID to match your custom originator, \
             or unset VTCODE_OPENAI_OAUTH_ORIGINATOR to use the default Codex identity."
        ),
        (None, None) => Ok(OAuthClientIdentity {
            client_id: DEFAULT_OPENAI_CLIENT_ID.to_string(),
            originator: DEFAULT_OPENAI_ORIGINATOR.to_string(),
        }),
    }
}

/// Stored OpenAI ChatGPT subscription session.
///
/// Custom `Debug` redacts all token fields to prevent credential leakage
/// through `tracing::debug!(?session)` or error wrappers.
#[derive(Clone, Serialize, Deserialize)]
pub struct OpenAIChatGptSession {
    /// Exchanged OpenAI bearer token used for normal API calls when available.
    /// If unavailable, VT Code falls back to the OAuth access token.
    pub openai_api_key: String,
    /// OAuth ID token from the sign-in flow.
    pub id_token: String,
    /// OAuth access token from the sign-in flow.
    pub access_token: String,
    /// Refresh token used to renew the session.
    pub refresh_token: String,
    /// ChatGPT workspace/account identifier, if present.
    pub account_id: Option<String>,
    /// Account email, if present.
    pub email: Option<String>,
    /// ChatGPT plan type, if present.
    pub plan: Option<String>,
    /// When the session was originally created.
    pub obtained_at: u64,
    /// When the OAuth/API-key exchange was last refreshed.
    pub refreshed_at: u64,
    /// Access-token expiry, if supplied by the authority.
    pub expires_at: Option<u64>,
}

impl fmt::Debug for OpenAIChatGptSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAIChatGptSession")
            .field("openai_api_key", &"<redacted>")
            .field("id_token", &"<redacted>")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("account_id", &self.account_id)
            .field("email", &self.email)
            .field("plan", &self.plan)
            .field("obtained_at", &self.obtained_at)
            .field("refreshed_at", &self.refreshed_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl OpenAIChatGptSession {
    fn is_refresh_due(&self) -> bool {
        let now = now_secs();
        if let Some(expires_at) = self.expires_at
            && now.saturating_add(REFRESH_SKEW_SECS) >= expires_at
        {
            return true;
        }
        now.saturating_sub(self.refreshed_at) >= REFRESH_INTERVAL_SECS
    }
}

/// Host-provided refresher for externally managed ChatGPT auth tokens.
#[async_trait]
pub trait OpenAIChatGptSessionRefresher: Send + Sync {
    async fn refresh_session(&self, current: &OpenAIChatGptSession) -> Result<OpenAIChatGptSession>;
}

#[derive(Clone)]
enum OpenAIChatGptAuthRefreshStrategy {
    Stored {
        storage_mode: AuthCredentialsStoreMode,
    },
    External {
        refresher: Arc<dyn OpenAIChatGptSessionRefresher>,
    },
}

impl fmt::Debug for OpenAIChatGptAuthRefreshStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stored { storage_mode } => f.debug_struct("Stored").field("storage_mode", storage_mode).finish(),
            Self::External { .. } => f.debug_struct("External").finish_non_exhaustive(),
        }
    }
}

/// Runtime auth state shared by OpenAI provider instances.
#[derive(Clone)]
pub struct OpenAIChatGptAuthHandle {
    session: Arc<Mutex<OpenAIChatGptSession>>,
    refresh_gate: Arc<AsyncMutex<()>>,
    auto_refresh: bool,
    refresh_strategy: OpenAIChatGptAuthRefreshStrategy,
}

impl fmt::Debug for OpenAIChatGptAuthHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAIChatGptAuthHandle")
            .field("auto_refresh", &self.auto_refresh)
            .field("refresh_strategy", &self.refresh_strategy)
            .finish()
    }
}

impl OpenAIChatGptAuthHandle {
    pub fn new(
        session: OpenAIChatGptSession,
        auth_config: OpenAIAuthConfig,
        storage_mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
            refresh_gate: Arc::new(AsyncMutex::new(())),
            auto_refresh: auth_config.auto_refresh,
            refresh_strategy: OpenAIChatGptAuthRefreshStrategy::Stored { storage_mode },
        }
    }

    pub fn new_external(
        session: OpenAIChatGptSession,
        auto_refresh: bool,
        refresher: Arc<dyn OpenAIChatGptSessionRefresher>,
    ) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
            refresh_gate: Arc::new(AsyncMutex::new(())),
            auto_refresh,
            refresh_strategy: OpenAIChatGptAuthRefreshStrategy::External { refresher },
        }
    }

    pub fn snapshot(&self) -> Result<OpenAIChatGptSession> {
        self.session
            .lock()
            .map(|guard| guard.clone())
            .map_err(|_| anyhow!("openai chatgpt auth mutex poisoned"))
    }

    pub fn current_api_key(&self) -> Result<String> {
        self.snapshot().map(|session| active_api_bearer_token(&session).to_string())
    }

    pub fn provider_label(&self) -> &'static str {
        "OpenAI (ChatGPT)"
    }

    pub async fn refresh_if_needed(&self) -> Result<()> {
        if !self.auto_refresh {
            return Ok(());
        }

        self.refresh_when(|session| session.is_refresh_due()).await
    }

    pub async fn force_refresh(&self) -> Result<()> {
        self.refresh_when(|_| true).await
    }

    async fn refresh_when<P>(&self, should_refresh: P) -> Result<()>
    where
        P: FnOnce(&OpenAIChatGptSession) -> bool,
    {
        let _refresh_guard = self.refresh_gate.lock().await;
        let session = self.snapshot()?;
        if !should_refresh(&session) {
            return Ok(());
        }

        let refreshed = match &self.refresh_strategy {
            OpenAIChatGptAuthRefreshStrategy::Stored { storage_mode } => {
                refresh_openai_chatgpt_session_from_snapshot(&session, *storage_mode).await?
            }
            OpenAIChatGptAuthRefreshStrategy::External { refresher } => refresher.refresh_session(&session).await?,
        };
        self.replace_session(refreshed)
    }

    #[must_use]
    fn using_external_tokens(&self) -> bool {
        matches!(self.refresh_strategy, OpenAIChatGptAuthRefreshStrategy::External { .. })
    }

    fn replace_session(&self, session: OpenAIChatGptSession) -> Result<()> {
        let mut guard = self.session.lock().map_err(|_| anyhow!("openai chatgpt auth mutex poisoned"))?;
        *guard = session;
        Ok(())
    }
}

/// OpenAI auth resolution chosen for the current runtime.
///
/// Custom `Debug` redacts the bearer `api_key` to prevent credential leakage.
#[derive(Clone)]
pub enum OpenAIResolvedAuth {
    ApiKey {
        api_key: String,
    },
    ChatGpt {
        api_key: String,
        handle: OpenAIChatGptAuthHandle,
    },
}

impl fmt::Debug for OpenAIResolvedAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey { .. } => f.debug_struct("OpenAIResolvedAuth::ApiKey").finish(),
            Self::ChatGpt { .. } => f.debug_struct("OpenAIResolvedAuth::ChatGpt").finish(),
        }
    }
}

impl OpenAIResolvedAuth {
    pub fn api_key(&self) -> &str {
        match self {
            Self::ApiKey { api_key } => api_key,
            Self::ChatGpt { api_key, .. } => api_key,
        }
    }

    pub fn handle(&self) -> Option<OpenAIChatGptAuthHandle> {
        match self {
            Self::ApiKey { .. } => None,
            Self::ChatGpt { handle, .. } => Some(handle.clone()),
        }
    }

    fn using_chatgpt(&self) -> bool {
        matches!(self, Self::ChatGpt { .. })
    }
}

fn active_api_bearer_token(session: &OpenAIChatGptSession) -> &str {
    if session.openai_api_key.trim().is_empty() {
        session.access_token.as_str()
    } else {
        session.openai_api_key.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIResolvedAuthSource {
    ApiKey,
    ChatGpt,
}

/// Where the ChatGPT session originated — used by CLI/TUI to render accurate
/// status without directly inspecting the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIChatGptSessionProvenance {
    /// Session stored in VT Code's own credential storage (full auto-refresh).
    Native,
    /// Session loaded from Codex CLI's `~/.codex/auth.json` (managed by Codex).
    CodexFallback,
}

/// Redacted summary of available OpenAI credentials for CLI/TUI display.
///
/// Does NOT carry token data — only metadata (email, plan, provenance, expiry)
/// so credential values can never leak through `Debug` or logging.
#[derive(Debug, Clone)]
pub struct OpenAICredentialOverview {
    pub api_key_available: bool,
    /// Email from the ChatGPT session's ID token, if available.
    pub chatgpt_email: Option<String>,
    /// Plan type from the ChatGPT session's ID token, if available.
    pub chatgpt_plan: Option<String>,
    /// `true` when a ChatGPT session (native or Codex fallback) is available.
    pub chatgpt_session_present: bool,
    /// Provenance of the ChatGPT session — `None` when no session is available.
    pub chatgpt_session_provenance: Option<OpenAIChatGptSessionProvenance>,
    /// `true` only when Codex's auth.json was **successfully parsed** into a
    /// usable session (not merely that the file exists on disk).
    pub codex_fallback_available: bool,
    pub active_source: Option<OpenAIResolvedAuthSource>,
    pub preferred_method: OpenAIPreferredMethod,
    pub notice: Option<String>,
    pub recommendation: Option<String>,
}

/// Generic auth status reused by slash auth/status output.
#[derive(Debug, Clone)]
pub enum OpenAIChatGptAuthStatus {
    Authenticated {
        label: Option<String>,
        age_seconds: u64,
        expires_in: Option<u64>,
    },
    NotAuthenticated,
}

/// Build the OpenAI ChatGPT OAuth authorization URL.
pub fn get_openai_chatgpt_auth_url(challenge: &PkceChallenge, callback_port: u16, state: &str) -> Result<String> {
    let redirect_uri = format!("http://localhost:{callback_port}{OPENAI_CALLBACK_PATH}");
    let identity = resolve_oauth_client_identity()?;
    let query = [
        ("response_type", "code".to_string()),
        ("client_id", identity.client_id.clone()),
        ("redirect_uri", redirect_uri),
        ("scope", "openid profile email offline_access api.connectors.read api.connectors.invoke".to_string()),
        ("code_challenge", challenge.code_challenge.clone()),
        ("code_challenge_method", challenge.code_challenge_method.clone()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("state", state.to_string()),
        ("originator", identity.originator),
    ];

    let encoded = query
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencoding::encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!("{OPENAI_AUTH_URL}?{encoded}"))
}

pub fn generate_openai_oauth_state() -> Result<String> {
    let mut state_bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut state_bytes)
        .map_err(|_| anyhow!("failed to generate openai oauth state"))?;
    Ok(URL_SAFE_NO_PAD.encode(state_bytes))
}

pub fn parse_openai_chatgpt_manual_callback_input(input: &str, expected_state: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("missing authorization callback input");
    }

    let query = if trimmed.contains("://") {
        let url = reqwest::Url::parse(trimmed).context("invalid callback url")?;
        url.query()
            .ok_or_else(|| anyhow!("callback url did not include a query string"))?
            .to_string()
    } else if trimmed.contains('=') {
        trimmed.trim_start_matches('?').to_string()
    } else {
        bail!("paste the full redirect url or query string containing code and state");
    };

    let code = extract_query_value(&query, "code")
        .ok_or_else(|| anyhow!("callback input did not include an authorization code"))?;
    let state = extract_query_value(&query, "state").ok_or_else(|| anyhow!("callback input did not include state"))?;
    if state != expected_state {
        bail!("OAuth error: state mismatch");
    }
    Ok(code)
}

/// Exchange an authorization code for OAuth tokens.
pub async fn exchange_openai_chatgpt_code_for_tokens(
    code: &str,
    challenge: &PkceChallenge,
    callback_port: u16,
) -> Result<OpenAIChatGptSession> {
    let redirect_uri = format!("http://localhost:{callback_port}{OPENAI_CALLBACK_PATH}");
    let identity = resolve_oauth_client_identity()?;
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&identity.client_id),
        urlencoding::encode(&challenge.code_verifier),
    );

    let token_response: OpenAITokenResponse = Client::new()
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("failed to exchange openai authorization code")?
        .error_for_status()
        .context("openai authorization-code exchange failed")?
        .json()
        .await
        .context("failed to parse openai authorization-code response")?;

    build_session_from_token_response(token_response).await
}

/// Resolve the active OpenAI auth source for the current configuration.
pub fn resolve_openai_auth(
    auth_config: &OpenAIAuthConfig,
    storage_mode: AuthCredentialsStoreMode,
    api_key: Option<String>,
) -> Result<OpenAIResolvedAuth> {
    crate::auth_service::OpenAIAccountAuthService::new(auth_config.clone(), storage_mode).resolve_runtime_auth(api_key)
}

pub fn summarize_openai_credentials(
    auth_config: &OpenAIAuthConfig,
    storage_mode: AuthCredentialsStoreMode,
    api_key: Option<String>,
) -> Result<OpenAICredentialOverview> {
    crate::auth_service::OpenAIAccountAuthService::new(auth_config.clone(), storage_mode).summarize_credentials(api_key)
}

pub fn save_openai_chatgpt_session(session: &OpenAIChatGptSession) -> Result<()> {
    save_openai_chatgpt_session_with_mode(session, AuthCredentialsStoreMode::default())
}

pub fn save_openai_chatgpt_session_with_mode(
    session: &OpenAIChatGptSession,
    mode: AuthCredentialsStoreMode,
) -> Result<()> {
    OpenAiSessionStorage::new().save(session, mode)
}

pub fn load_openai_chatgpt_session() -> Result<Option<OpenAIChatGptSession>> {
    OpenAiSessionStorage::new().load(AuthCredentialsStoreMode::Keyring)
}

pub fn load_openai_chatgpt_session_with_mode(mode: AuthCredentialsStoreMode) -> Result<Option<OpenAIChatGptSession>> {
    OpenAiSessionStorage::new().load(mode)
}

pub fn clear_openai_chatgpt_session() -> Result<()> {
    OpenAiSessionStorage::new().clear_all()
}

pub fn clear_openai_chatgpt_session_with_mode(mode: AuthCredentialsStoreMode) -> Result<()> {
    OpenAiSessionStorage::new().clear(mode)
}

pub fn get_openai_chatgpt_auth_status() -> Result<OpenAIChatGptAuthStatus> {
    get_openai_chatgpt_auth_status_with_mode(AuthCredentialsStoreMode::default())
}

pub fn get_openai_chatgpt_auth_status_with_mode(mode: AuthCredentialsStoreMode) -> Result<OpenAIChatGptAuthStatus> {
    let Some(session) = load_openai_chatgpt_session_with_mode(mode)? else {
        return Ok(OpenAIChatGptAuthStatus::NotAuthenticated);
    };
    let now = now_secs();
    Ok(OpenAIChatGptAuthStatus::Authenticated {
        label: session
            .email
            .clone()
            .or_else(|| session.plan.clone())
            .or_else(|| session.account_id.clone()),
        age_seconds: now.saturating_sub(session.obtained_at),
        expires_in: session.expires_at.map(|expires_at| expires_at.saturating_sub(now)),
    })
}

pub async fn refresh_openai_chatgpt_session_with_mode(mode: AuthCredentialsStoreMode) -> Result<OpenAIChatGptSession> {
    let session = load_openai_chatgpt_session_with_mode(mode)?.ok_or_else(|| anyhow!("Run vtcode login openai"))?;
    refresh_openai_chatgpt_session_from_snapshot(&session, mode).await
}

async fn refresh_openai_chatgpt_session_from_snapshot(
    session: &OpenAIChatGptSession,
    storage_mode: AuthCredentialsStoreMode,
) -> Result<OpenAIChatGptSession> {
    let _lock = acquire_refresh_lock().await?;
    if let Some(current) = load_openai_chatgpt_session_with_mode(storage_mode)?
        && session_has_newer_refresh_state(&current, session)
    {
        return Ok(current);
    }
    refresh_openai_chatgpt_session_without_lock(session, storage_mode).await
}

/// Refresh the ChatGPT session using the stored refresh token.
///
/// The response is parsed as [`OpenAIRefreshResponse`] with independently
/// optional fields — OpenAI's token endpoint may omit unchanged fields.
/// Omitted fields preserve the current session's values. This matches the
/// behaviour of `openai/codex`'s `RefreshResponse` + `persist_tokens`.
async fn refresh_openai_chatgpt_session_without_lock(
    current: &OpenAIChatGptSession,
    storage_mode: AuthCredentialsStoreMode,
) -> Result<OpenAIChatGptSession> {
    let identity = resolve_oauth_client_identity()?;
    let response = Client::new()
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&client_id={}&refresh_token={}",
            urlencoding::encode(&identity.client_id),
            urlencoding::encode(&current.refresh_token),
        ))
        .send()
        .await
        .context("failed to refresh openai chatgpt token")?;

    // Check for HTTP errors. Unlike error_for_status_ref(), we capture the
    // response body to classify token-endpoint errors (e.g. invalid_grant,
    // refresh_token_expired) that reqwest's status-only error would miss.
    if !response.status().is_success() {
        let status = response.status();
        // Read a bounded body for error classification — never log it raw.
        let body_text = read_bounded_text(response, MAX_ERROR_BODY_BYTES).await;
        return Err(classify_refresh_status_error(status, &body_text));
    }

    let refresh_response: OpenAIRefreshResponse =
        response.json().await.context("failed to parse openai refresh response")?;

    let session = merge_refresh_response(current, refresh_response).await?;
    // Guard against a blank access_token — the primary bearer credential.
    // This protects the minimal-session refresh helper (which starts with
    // blank token fields) from persisting a session with blank tokens when
    // the token endpoint returns a partial response that omits access_token.
    if session.access_token.trim().is_empty() {
        bail!("openai token refresh returned no access token — the session cannot be used");
    }
    save_openai_chatgpt_session_with_mode(&session, storage_mode)?;
    Ok(session)
}

/// Merge a partial refresh response into the current session, preserving
/// omitted fields. Only re-exchanges the API key when a new `id_token` is
/// present; otherwise keeps the previous exchanged key.
async fn merge_refresh_response(
    current: &OpenAIChatGptSession,
    resp: OpenAIRefreshResponse,
) -> Result<OpenAIChatGptSession> {
    let now = now_secs();
    // Track which fields were present before moving them out of resp.
    // Treat blank-string values as absent — some token endpoints return
    // empty strings for omitted fields rather than leaving them out.
    let has_new_id_token = resp.id_token.as_deref().is_some_and(|v| !v.trim().is_empty());
    let has_new_access_token = resp.access_token.as_deref().is_some_and(|v| !v.trim().is_empty());
    let new_id_token = resp
        .id_token
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| current.id_token.clone());
    let new_access_token = resp
        .access_token
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| current.access_token.clone());
    let new_refresh_token = resp
        .refresh_token
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| current.refresh_token.clone());

    // Re-exchange the API key only when a new id_token was provided.
    let openai_api_key = if has_new_id_token {
        match exchange_openai_chatgpt_api_key(&new_id_token).await {
            Ok(api_key) => api_key,
            Err(err) => {
                tracing::warn!("openai api-key exchange unavailable, falling back to previous key: {err}");
                current.openai_api_key.clone()
            }
        }
    } else {
        current.openai_api_key.clone()
    };

    // Recompute expiry: prefer expires_in from the response, then try to parse
    // exp from the new access_token JWT. For a **changed** access token without
    // expires_in or a parseable exp, set None — do NOT inherit the previous
    // token's expiry, which belongs to a different token.
    //
    // Key distinction: `has_new_access_token` means the field was present and
    // non-blank, NOT that the token value changed. If the endpoint repeats the
    // same opaque access token without expires_in, the old expiry is still
    // valid and must be preserved.
    let access_token_changed = has_new_access_token && new_access_token != current.access_token;
    let expires_at = if let Some(secs) = resp.expires_in {
        Some(now.saturating_add(secs))
    } else if access_token_changed {
        parse_jwt_exp(&new_access_token)
    } else {
        current.expires_at
    };

    // Update email/plan/account_id only when a new id_token was provided.
    let (email, plan, account_id) = if has_new_id_token {
        let id_claims = parse_jwt_claims(&new_id_token)?;
        let access_claims = parse_jwt_claims(&new_access_token).ok();
        let email = id_claims.email.clone();
        let plan = access_claims.as_ref().and_then(|c| c.plan.clone()).or(id_claims.plan);
        let account_id = access_claims
            .as_ref()
            .and_then(|c| c.account_id.clone())
            .or(id_claims.account_id);
        (email, plan, account_id)
    } else {
        (current.email.clone(), current.plan.clone(), current.account_id.clone())
    };

    Ok(OpenAIChatGptSession {
        openai_api_key,
        id_token: new_id_token,
        access_token: new_access_token,
        refresh_token: new_refresh_token,
        account_id,
        email,
        plan,
        // Preserve the original obtained_at — only refreshed_at advances.
        obtained_at: current.obtained_at,
        refreshed_at: now,
        expires_at,
    })
}

async fn build_session_from_token_response(token_response: OpenAITokenResponse) -> Result<OpenAIChatGptSession> {
    // Validate that the token response contains usable credentials.
    if token_response.access_token.trim().is_empty() {
        bail!("openai authorization-code response did not include a usable access token");
    }
    if token_response.refresh_token.trim().is_empty() {
        bail!("openai authorization-code response did not include a usable refresh token");
    }
    let id_claims = parse_jwt_claims(&token_response.id_token)?;
    let access_claims = parse_jwt_claims(&token_response.access_token).ok();
    let api_key = match exchange_openai_chatgpt_api_key(&token_response.id_token).await {
        Ok(api_key) => api_key,
        Err(err) => {
            tracing::warn!("openai api-key exchange unavailable, falling back to oauth access token: {err}");
            String::new()
        }
    };
    let now = now_secs();
    Ok(OpenAIChatGptSession {
        openai_api_key: api_key,
        id_token: token_response.id_token,
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        account_id: access_claims
            .as_ref()
            .and_then(|claims| claims.account_id.clone())
            .or(id_claims.account_id),
        email: id_claims
            .email
            .or_else(|| access_claims.as_ref().and_then(|claims| claims.email.clone())),
        plan: access_claims.as_ref().and_then(|claims| claims.plan.clone()).or(id_claims.plan),
        obtained_at: now,
        refreshed_at: now,
        expires_at: token_response.expires_in.map(|secs| now.saturating_add(secs)),
    })
}

async fn exchange_openai_chatgpt_api_key(id_token: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct ExchangeResponse {
        access_token: String,
    }

    let identity = resolve_oauth_client_identity()?;
    let exchange: ExchangeResponse = Client::new()
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type={}&client_id={}&requested_token={}&subject_token={}&subject_token_type={}",
            urlencoding::encode("urn:ietf:params:oauth:grant-type:token-exchange"),
            urlencoding::encode(&identity.client_id),
            urlencoding::encode("openai-api-key"),
            urlencoding::encode(id_token),
            urlencoding::encode("urn:ietf:params:oauth:token-type:id_token"),
        ))
        .send()
        .await
        .context("failed to exchange openai id token for api key")?
        .error_for_status()
        .context("openai api-key exchange failed")?
        .json()
        .await
        .context("failed to parse openai api-key exchange response")?;

    Ok(exchange.access_token)
}

#[derive(Deserialize)]
struct OpenAITokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Refresh-token grant response — all token fields are independently optional
/// because OpenAI's token endpoint may omit unchanged fields (matching the
/// behaviour observed in `openai/codex`'s `RefreshResponse`). Omitted fields
/// preserve the previous session's values during merge.
#[derive(Deserialize)]
struct OpenAIRefreshResponse {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<ProfileClaims>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Debug, Deserialize)]
struct ProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_plan_type: Option<String>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ParsedIdTokenClaims {
    pub(crate) email: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) plan: Option<String>,
}

pub(crate) fn parse_jwt_claims(jwt: &str) -> Result<ParsedIdTokenClaims> {
    let mut parts = jwt.split('.');
    let (_, payload_b64, _) = match (parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature))
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            (header, payload, signature)
        }
        _ => bail!("invalid openai id token"),
    };

    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .context("failed to decode openai id token payload")?;
    let claims: IdTokenClaims = serde_json::from_slice(&payload).context("failed to parse openai id token payload")?;

    Ok(ParsedIdTokenClaims {
        email: claims.email.or_else(|| claims.profile.and_then(|profile| profile.email)),
        account_id: claims.auth.as_ref().and_then(|auth| auth.chatgpt_account_id.clone()),
        plan: claims.auth.and_then(|auth| auth.chatgpt_plan_type),
    })
}

/// Extract the standard `exp` (expiry) claim from a JWT, if present.
///
/// Returns `None` when the token is not a JWT or has no `exp` claim.
/// This is used to populate `expires_at` for Codex-imported sessions,
/// since Codex's `auth.json` does not store expiry separately.
pub(crate) fn parse_jwt_exp(jwt: &str) -> Option<u64> {
    let mut parts = jwt.split('.');
    let _ = parts.next()?;
    let payload_b64 = parts.next()?;
    if payload_b64.is_empty() {
        return None;
    }
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    #[derive(Deserialize)]
    struct ExpClaim {
        #[serde(default)]
        exp: Option<u64>,
    }
    let claims: ExpClaim = serde_json::from_slice(&payload).ok()?;
    claims.exp
}

fn extract_query_value(query: &str, key: &str) -> Option<String> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter_map(|pair| {
            let (pair_key, pair_value) = pair.split_once('=')?;
            (pair_key == key)
                .then(|| urlencoding::decode(pair_value).ok().map(|value| value.into_owned()))
                .flatten()
        })
        .find(|value| !value.is_empty())
}

fn session_has_newer_refresh_state(current: &OpenAIChatGptSession, previous: &OpenAIChatGptSession) -> bool {
    current.refresh_token != previous.refresh_token
        || current.refreshed_at > previous.refreshed_at
        || current.obtained_at > previous.obtained_at
}

struct RefreshLockGuard {
    file: fs::File,
}

impl Drop for RefreshLockGuard {
    fn drop(&mut self) {
        drop(FileExt::unlock(&self.file));
    }
}

async fn acquire_refresh_lock() -> Result<RefreshLockGuard> {
    let path = auth_storage_dir()?.join(OPENAI_REFRESH_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .context("failed to open openai refresh lock")?;
    let file = tokio::task::spawn_blocking(move || {
        file.lock_exclusive().context("failed to acquire openai refresh lock")?;
        Ok::<_, anyhow::Error>(file)
    })
    .await
    .context("openai refresh lock task failed")??;
    Ok(RefreshLockGuard { file })
}

/// Read at most `max_bytes` from an HTTP response body as a string.
///
/// Reads the response in chunks and stops once `max_bytes` have been
/// accumulated, preventing unbounded memory allocation from a misbehaving or
/// hostile endpoint. Invalid UTF-8 sequences are replaced (lossy) since we
/// only use the text for best-effort error classification, never for display
/// or logging.
async fn read_bounded_text(mut response: reqwest::Response, max_bytes: usize) -> String {
    let mut buf = Vec::with_capacity(max_bytes.min(8 * 1024));
    while buf.len() < max_bytes {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = max_bytes - buf.len();
                if chunk.len() <= remaining {
                    buf.extend_from_slice(&chunk);
                } else {
                    buf.extend_from_slice(chunk.get(..remaining).unwrap_or_default());
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn classify_refresh_status_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let failure = classify_refresh_failure(status, body);
    if failure.action() == RefreshFailureAction::ClearStoredSession {
        if let Err(clear_err) = clear_session_from_all_stores() {
            tracing::warn!("failed to clear expired openai chatgpt session across all stores: {clear_err}");
        }
    }
    failure.into_error()
}

fn clear_session_from_all_stores() -> Result<()> {
    OpenAiSessionStorage::new().clear_all()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthCallbackOutcome;
    use crate::generate_pkce_challenge;
    use assert_fs::TempDir;
    use serial_test::serial;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct ExternalRefresher;

    #[async_trait]
    impl OpenAIChatGptSessionRefresher for ExternalRefresher {
        async fn refresh_session(&self, current: &OpenAIChatGptSession) -> Result<OpenAIChatGptSession> {
            let mut refreshed = current.clone();
            refreshed.access_token = "oauth-access-refreshed".to_string();
            refreshed.refreshed_at = current.refreshed_at.saturating_add(1);
            refreshed.expires_at = Some(now_secs() + 3600);
            Ok(refreshed)
        }
    }

    struct TestAuthDirGuard {
        temp_dir: Option<TempDir>,
        codex_temp_dir: Option<TempDir>,
        previous: Option<PathBuf>,
        previous_codex_home: Option<String>,
    }

    impl TestAuthDirGuard {
        fn new() -> Self {
            let temp_dir = TempDir::new().expect("create temp auth dir");
            let previous = crate::storage_paths::auth_storage_dir_override_for_tests().expect("read auth dir override");
            crate::storage_paths::set_auth_storage_dir_override_for_tests(Some(temp_dir.path().to_path_buf()))
                .expect("set temp auth dir override");

            // Isolate CODEX_HOME so the Codex auth.json fallback doesn't pick
            // up a real Codex session from the user's machine during tests.
            let codex_temp_dir = TempDir::new().expect("create temp codex home");
            let previous_codex_home = std::env::var("CODEX_HOME").ok();
            vtcode_commons::env_lock::set_var("CODEX_HOME", codex_temp_dir.path());

            Self {
                temp_dir: Some(temp_dir),
                codex_temp_dir: Some(codex_temp_dir),
                previous,
                previous_codex_home,
            }
        }
    }

    impl Drop for TestAuthDirGuard {
        fn drop(&mut self) {
            crate::storage_paths::set_auth_storage_dir_override_for_tests(self.previous.clone())
                .expect("restore auth dir override");
            if let Some(temp_dir) = self.temp_dir.take() {
                temp_dir.close().expect("remove temp auth dir");
            }
            vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", self.previous_codex_home.as_deref());
            if let Some(codex_temp_dir) = self.codex_temp_dir.take() {
                codex_temp_dir.close().expect("remove temp codex home");
            }
        }
    }

    fn sample_session() -> OpenAIChatGptSession {
        OpenAIChatGptSession {
            openai_api_key: "api-key".to_string(),
            id_token: "aGVhZGVy.eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjXzEyMyIsImNoYXRncHRfcGxhbl90eXBlIjoicGx1cyJ9fQ.sig".to_string(),
            access_token: "oauth-access".to_string(),
            refresh_token: "refresh-token".to_string(),
            account_id: Some("acc_123".to_string()),
            email: Some("test@example.com".to_string()),
            plan: Some("plus".to_string()),
            obtained_at: 10,
            refreshed_at: 10,
            expires_at: Some(now_secs() + 3600),
        }
    }

    #[test]
    fn auth_url_contains_expected_openai_parameters() {
        // RAII guard locks env and restores both vars on drop (panic-safe).
        let env = OauthEnvGuard::new();
        env.remove_client_id();
        env.remove_originator();

        let challenge = PkceChallenge {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
            code_challenge_method: "S256".to_string(),
        };

        let url = get_openai_chatgpt_auth_url(&challenge, 1455, "test-state").expect("auth url");
        assert!(url.starts_with(OPENAI_AUTH_URL));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("state=test-state"));
    }

    #[test]
    fn auth_url_honors_custom_client_id_env_override() {
        // RAII guard locks env and restores both vars on drop (panic-safe).
        let env = OauthEnvGuard::new();
        env.set_client_id("app_custom_override");
        env.set_originator("vtcode_custom");

        let challenge = PkceChallenge {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
            code_challenge_method: "S256".to_string(),
        };
        let url = get_openai_chatgpt_auth_url(&challenge, 1455, "test-state").expect("auth url");
        assert!(url.contains("client_id=app_custom_override"), "custom client_id not used: {url}");
        assert!(url.contains("originator=vtcode_custom"), "custom originator not used: {url}");
        assert!(!url.contains("app_EMoamEEZ73f0CkXaXp7hrann"), "default client_id leaked through override: {url}");
    }

    /// RAII guard that locks the environment and restores both OAuth identity
    /// env vars on drop — even if the test panics. This ensures parallel
    /// tests don't leak env mutations to each other.
    struct OauthEnvGuard {
        env: vtcode_commons::env_lock::EnvGuard,
        prev_client_id: Option<std::ffi::OsString>,
        prev_originator: Option<std::ffi::OsString>,
    }

    impl OauthEnvGuard {
        fn new() -> Self {
            let env = vtcode_commons::env_lock::lock();
            let prev_client_id = std::env::var_os("VTCODE_OPENAI_OAUTH_CLIENT_ID");
            let prev_originator = std::env::var_os("VTCODE_OPENAI_OAUTH_ORIGINATOR");
            Self { env, prev_client_id, prev_originator }
        }

        fn set_client_id(&self, value: &str) {
            self.env.set_var("VTCODE_OPENAI_OAUTH_CLIENT_ID", value);
        }

        fn set_originator(&self, value: &str) {
            self.env.set_var("VTCODE_OPENAI_OAUTH_ORIGINATOR", value);
        }

        fn remove_client_id(&self) {
            self.env.remove_var("VTCODE_OPENAI_OAUTH_CLIENT_ID");
        }

        fn remove_originator(&self) {
            self.env.remove_var("VTCODE_OPENAI_OAUTH_ORIGINATOR");
        }
    }

    impl Drop for OauthEnvGuard {
        fn drop(&mut self) {
            self.env
                .restore_var("VTCODE_OPENAI_OAUTH_CLIENT_ID", self.prev_client_id.take());
            self.env
                .restore_var("VTCODE_OPENAI_OAUTH_ORIGINATOR", self.prev_originator.take());
        }
    }

    #[test]
    fn resolve_oauth_client_identity_both_defaults() {
        let env = OauthEnvGuard::new();
        env.remove_client_id();
        env.remove_originator();

        let identity = resolve_oauth_client_identity().expect("defaults");
        assert_eq!(identity.client_id, DEFAULT_OPENAI_CLIENT_ID);
        assert_eq!(identity.originator, DEFAULT_OPENAI_ORIGINATOR);
    }

    #[test]
    fn resolve_oauth_client_identity_both_custom() {
        let env = OauthEnvGuard::new();
        env.set_client_id("app_custom");
        env.set_originator("my_originator");

        let identity = resolve_oauth_client_identity().expect("custom pair");
        assert_eq!(identity.client_id, "app_custom");
        assert_eq!(identity.originator, "my_originator");
    }

    #[test]
    fn resolve_oauth_client_identity_only_client_id_is_error() {
        let env = OauthEnvGuard::new();
        env.set_client_id("app_custom");
        env.remove_originator();

        let err = resolve_oauth_client_identity().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("VTCODE_OPENAI_OAUTH_CLIENT_ID"), "error should name the set var: {msg}");
        assert!(msg.contains("VTCODE_OPENAI_OAUTH_ORIGINATOR"), "error should name the missing var: {msg}");
    }

    #[test]
    fn resolve_oauth_client_identity_only_originator_is_error() {
        let env = OauthEnvGuard::new();
        env.remove_client_id();
        env.set_originator("my_originator");

        let err = resolve_oauth_client_identity().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("VTCODE_OPENAI_OAUTH_ORIGINATOR"), "error should name the set var: {msg}");
        assert!(msg.contains("VTCODE_OPENAI_OAUTH_CLIENT_ID"), "error should name the missing var: {msg}");
    }

    #[test]
    fn resolve_oauth_client_identity_blank_values_treated_as_unset() {
        let env = OauthEnvGuard::new();
        env.set_client_id("   ");
        env.set_originator("  ");

        // Blank values are treated as unset → defaults used.
        let identity = resolve_oauth_client_identity().expect("defaults from blank");
        assert_eq!(identity.client_id, DEFAULT_OPENAI_CLIENT_ID);
        assert_eq!(identity.originator, DEFAULT_OPENAI_ORIGINATOR);
    }

    #[test]
    fn parse_jwt_claims_extracts_openai_claims() {
        let claims = parse_jwt_claims(
            "aGVhZGVy.eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjXzEyMyIsImNoYXRncHRfcGxhbl90eXBlIjoicGx1cyJ9fQ.sig",
        )
        .expect("claims");
        assert_eq!(claims.email.as_deref(), Some("test@example.com"));
        assert_eq!(claims.account_id.as_deref(), Some("acc_123"));
        assert_eq!(claims.plan.as_deref(), Some("plus"));
    }

    #[test]
    fn session_refresh_due_uses_expiry_and_age() {
        let mut session = sample_session();
        let now = now_secs();
        session.obtained_at = now;
        session.refreshed_at = now;
        session.expires_at = Some(now + 3600);
        assert!(!session.is_refresh_due());
        session.expires_at = Some(now);
        assert!(session.is_refresh_due());
    }

    #[tokio::test]
    #[serial]
    async fn external_auth_handle_refreshes_without_persisting_session() {
        let _guard = TestAuthDirGuard::new();
        let mut session = sample_session();
        session.openai_api_key.clear();
        session.expires_at = Some(now_secs().saturating_sub(1));
        let handle = OpenAIChatGptAuthHandle::new_external(session, true, Arc::new(ExternalRefresher));

        assert!(handle.using_external_tokens());
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load session")
                .is_none()
        );

        handle.force_refresh().await.expect("force refresh");

        assert_eq!(handle.current_api_key().expect("current api key"), "oauth-access-refreshed");
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load session")
                .is_none()
        );
    }

    struct CountingExternalRefresher {
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl OpenAIChatGptSessionRefresher for CountingExternalRefresher {
        async fn refresh_session(&self, current: &OpenAIChatGptSession) -> Result<OpenAIChatGptSession> {
            let mut calls = self.calls.lock().expect("refresh calls mutex should lock");
            *calls += 1;
            drop(calls);

            let mut refreshed = current.clone();
            refreshed.access_token = "oauth-access-refreshed".to_string();
            refreshed.refreshed_at = now_secs();
            refreshed.expires_at = Some(now_secs() + 3600);
            Ok(refreshed)
        }
    }

    #[tokio::test]
    async fn refresh_if_needed_serializes_external_refreshes() {
        let mut session = sample_session();
        session.openai_api_key.clear();
        session.expires_at = Some(now_secs().saturating_sub(1));
        let calls = Arc::new(Mutex::new(0usize));
        let handle = OpenAIChatGptAuthHandle::new_external(
            session,
            true,
            Arc::new(CountingExternalRefresher { calls: Arc::clone(&calls) }),
        );

        let first = handle.clone();
        let second = handle.clone();
        let (first_result, second_result) = tokio::join!(first.refresh_if_needed(), second.refresh_if_needed());

        first_result.expect("first refresh should succeed");
        second_result.expect("second refresh should succeed");
        assert_eq!(
            *calls.lock().expect("refresh calls mutex should lock"),
            1,
            "concurrent refresh_if_needed calls should share one refresh"
        );
        assert_eq!(handle.current_api_key().expect("current api key"), "oauth-access-refreshed");
    }

    #[test]
    #[serial]
    fn resolve_openai_auth_prefers_chatgpt_in_auto_permission() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");
        let resolved = resolve_openai_auth(
            &OpenAIAuthConfig::default(),
            AuthCredentialsStoreMode::File,
            Some("api-key".to_string()),
        )
        .expect("resolved auth");
        assert!(resolved.using_chatgpt());
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn file_storage_uses_private_permissions() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let _guard = TestAuthDirGuard::new();
        let session = sample_session();

        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");

        let metadata = fs::metadata(OpenAiSessionStorage::new().current_file_path().expect("session path"))
            .expect("read session metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    #[serial]
    fn legacy_file_session_migrates_to_shared_storage() {
        use std::fs;

        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        let encrypted = encrypt_session(&session).expect("encrypt legacy session");
        let legacy_path = get_session_path().expect("legacy session path");
        fs::write(&legacy_path, serde_json::to_vec(&encrypted).expect("serialize legacy session"))
            .expect("write legacy session");

        let loaded = load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
            .expect("load migrated session")
            .expect("session should be present");

        assert_eq!(loaded.account_id, session.account_id);
        assert!(legacy_path.exists(), "legacy session should remain as a rollback source after migration");
        assert!(
            OpenAiSessionStorage::new()
                .current_file_path()
                .expect("shared session path")
                .exists()
        );
    }

    #[test]
    #[serial]
    fn resolve_openai_auth_auto_falls_back_to_api_key_without_session() {
        let _guard = TestAuthDirGuard::new();
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
        let resolved = resolve_openai_auth(
            &OpenAIAuthConfig::default(),
            AuthCredentialsStoreMode::File,
            Some("api-key".to_string()),
        )
        .expect("resolved auth");
        assert!(matches!(resolved, OpenAIResolvedAuth::ApiKey { .. }));
    }

    #[test]
    #[serial]
    fn resolve_openai_auth_auto_rejects_blank_api_key_without_session() {
        let _guard = TestAuthDirGuard::new();
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
        let error =
            resolve_openai_auth(&OpenAIAuthConfig::default(), AuthCredentialsStoreMode::File, Some("   ".to_string()))
                .expect_err("blank api key should fail");
        assert!(error.to_string().contains("OpenAI API key not found"));
    }

    #[test]
    #[serial]
    fn resolve_openai_auth_api_key_mode_ignores_stored_chatgpt_session() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");
        let resolved = resolve_openai_auth(
            &OpenAIAuthConfig {
                preferred_method: OpenAIPreferredMethod::ApiKey,
                ..OpenAIAuthConfig::default()
            },
            AuthCredentialsStoreMode::File,
            Some("api-key".to_string()),
        )
        .expect("resolved auth");
        assert!(matches!(resolved, OpenAIResolvedAuth::ApiKey { .. }));
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
    }

    #[test]
    #[serial]
    fn resolve_openai_auth_chatgpt_mode_requires_stored_session() {
        let _guard = TestAuthDirGuard::new();
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
        let error = resolve_openai_auth(
            &OpenAIAuthConfig {
                preferred_method: OpenAIPreferredMethod::Chatgpt,
                ..OpenAIAuthConfig::default()
            },
            AuthCredentialsStoreMode::File,
            Some("api-key".to_string()),
        )
        .expect_err("chatgpt mode should require a stored session");
        assert!(error.to_string().contains("vtcode login openai"));
    }

    #[test]
    #[serial]
    fn summarize_openai_credentials_reports_dual_source_notice() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");
        let overview = summarize_openai_credentials(
            &OpenAIAuthConfig::default(),
            AuthCredentialsStoreMode::File,
            Some("api-key".to_string()),
        )
        .expect("overview");
        assert_eq!(overview.active_source, Some(OpenAIResolvedAuthSource::ChatGpt));
        assert!(overview.notice.is_some());
        assert!(overview.recommendation.is_some());
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
    }

    #[test]
    #[serial]
    fn summarize_openai_credentials_respects_api_key_preference() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");
        let overview = summarize_openai_credentials(
            &OpenAIAuthConfig {
                preferred_method: OpenAIPreferredMethod::ApiKey,
                ..OpenAIAuthConfig::default()
            },
            AuthCredentialsStoreMode::File,
            Some("api-key".to_string()),
        )
        .expect("overview");
        assert_eq!(overview.active_source, Some(OpenAIResolvedAuthSource::ApiKey));
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
    }

    #[test]
    fn encrypted_file_round_trip_restores_session() {
        let session = sample_session();
        let encrypted = encrypt_session(&session).expect("encrypt");
        let decrypted = decrypt_session(&encrypted).expect("decrypt");
        assert_eq!(decrypted.account_id, session.account_id);
        assert_eq!(decrypted.email, session.email);
        assert_eq!(decrypted.plan, session.plan);
    }

    #[test]
    #[serial]
    fn default_loader_falls_back_to_file_session() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");

        let loaded = load_openai_chatgpt_session()
            .expect("load session")
            .expect("stored session should be found");

        assert_eq!(loaded.account_id, session.account_id);
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
    }

    #[test]
    #[serial]
    fn keyring_mode_loader_falls_back_to_file_session() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save session");

        let loaded = load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::Keyring)
            .expect("load session")
            .expect("stored session should be found");

        assert_eq!(loaded.email, session.email);
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear session");
    }

    #[test]
    #[serial]
    fn clear_openai_chatgpt_session_removes_file_and_keyring_sessions() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save file session");

        if save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::Keyring).is_err() {
            clear_openai_chatgpt_session().expect("clear session");
            assert!(
                load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                    .expect("load file session")
                    .is_none()
            );
            return;
        }

        clear_openai_chatgpt_session().expect("clear session");
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
    }

    #[test]
    fn active_api_bearer_token_falls_back_to_access_token() {
        let mut session = sample_session();
        session.openai_api_key.clear();

        assert_eq!(active_api_bearer_token(&session), "oauth-access");
    }

    #[test]
    fn parse_manual_callback_input_accepts_full_redirect_url() {
        let code = parse_openai_chatgpt_manual_callback_input(
            "http://localhost:1455/auth/callback?code=auth-code&state=test-state",
            "test-state",
        )
        .expect("manual input should parse");
        assert_eq!(code, "auth-code");
    }

    #[test]
    fn parse_manual_callback_input_accepts_query_string() {
        let code = parse_openai_chatgpt_manual_callback_input("code=auth-code&state=test-state", "test-state")
            .expect("manual input should parse");
        assert_eq!(code, "auth-code");
    }

    #[test]
    fn parse_manual_callback_input_rejects_bare_code() {
        let error = parse_openai_chatgpt_manual_callback_input("auth-code", "test-state")
            .expect_err("bare code should be rejected");
        assert!(error.to_string().contains("full redirect url or query string"));
    }

    #[test]
    fn parse_manual_callback_input_rejects_state_mismatch() {
        let error = parse_openai_chatgpt_manual_callback_input("code=auth-code&state=wrong-state", "test-state")
            .expect_err("state mismatch should fail");
        assert!(error.to_string().contains("state mismatch"));
    }

    #[tokio::test]
    #[serial]
    async fn refresh_lock_serializes_parallel_acquisition() {
        let _guard = TestAuthDirGuard::new();
        let first = tokio::spawn(async {
            let _lock = acquire_refresh_lock().await.expect("first lock");
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let start = std::time::Instant::now();
        let second = tokio::spawn(async {
            let _lock = acquire_refresh_lock().await.expect("second lock");
        });

        first.await.expect("first task");
        second.await.expect("second task");
        assert!(start.elapsed() >= std::time::Duration::from_millis(100));
    }

    // ── Debug redaction tests ──

    #[test]
    fn debug_impl_redacts_all_token_fields() {
        let session = sample_session();
        let debug_str = format!("{session:?}");
        // None of the secret values may appear in the Debug output.
        assert!(!debug_str.contains("api-key"), "openai_api_key leaked: {debug_str}");
        assert!(!debug_str.contains("oauth-access"), "access_token leaked: {debug_str}");
        assert!(!debug_str.contains("refresh-token"), "refresh_token leaked: {debug_str}");
        // The id_token JWT body is long; check a distinctive substring.
        assert!(!debug_str.contains("eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20i"), "id_token leaked: {debug_str}");
        // Non-secret metadata should still be present.
        assert!(debug_str.contains("test@example.com"), "email should be visible: {debug_str}");
        assert!(debug_str.contains("plus"), "plan should be visible: {debug_str}");
    }

    #[test]
    fn debug_impl_redacts_resolved_auth_api_key() {
        let resolved = OpenAIResolvedAuth::ApiKey { api_key: "sk-secret-key".to_string() };
        let debug_str = format!("{resolved:?}");
        assert!(!debug_str.contains("sk-secret-key"), "api_key leaked: {debug_str}");
    }

    #[test]
    fn debug_impl_redacts_resolved_auth_chatgpt_handle() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        let handle = OpenAIChatGptAuthHandle::new(session, OpenAIAuthConfig::default(), AuthCredentialsStoreMode::File);
        let resolved = OpenAIResolvedAuth::ChatGpt { api_key: "sk-secret-bearer".to_string(), handle };
        let debug_str = format!("{resolved:?}");
        assert!(!debug_str.contains("sk-secret-bearer"), "bearer leaked: {debug_str}");
    }

    #[test]
    fn credential_overview_carries_no_token_fields() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");
        let overview = summarize_openai_credentials(
            &OpenAIAuthConfig::default(),
            AuthCredentialsStoreMode::File,
            Some("sk-overview-key".to_string()),
        )
        .expect("overview");
        // The overview is a display struct — verify it only carries metadata,
        // not the raw session or token strings.
        let debug_str = format!("{overview:?}");
        assert!(!debug_str.contains("oauth-access"), "access_token leaked: {debug_str}");
        assert!(!debug_str.contains("refresh-token"), "refresh_token leaked: {debug_str}");
        assert!(!debug_str.contains("api-key"), "openai_api_key leaked: {debug_str}");
        // The overview should not contain the raw API key value either.
        assert!(!debug_str.contains("sk-overview-key"), "api key value leaked: {debug_str}");
        clear_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("clear");
    }

    // ── Partial refresh response merge tests ──
    //
    // These test merge_refresh_response directly with resp.id_token = None,
    // which skips the HTTP API-key exchange (has_new_id_token = false).
    // This lets us verify field-preservation behaviour without network access.

    #[tokio::test]
    async fn merge_preserves_omitted_access_token() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: None,
            refresh_token: Some("new-refresh".to_string()),
            expires_in: Some(3600),
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.access_token, current.access_token, "omitted access_token should be preserved");
        assert_eq!(merged.refresh_token, "new-refresh");
    }

    #[tokio::test]
    async fn merge_preserves_omitted_refresh_token() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("new-access".to_string()),
            refresh_token: None,
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.refresh_token, current.refresh_token, "omitted refresh_token should be preserved");
        assert_eq!(merged.access_token, "new-access");
    }

    #[tokio::test]
    async fn merge_preserves_omitted_id_token_and_api_key() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("new-access".to_string()),
            refresh_token: None,
            expires_in: Some(1800),
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        // No new id_token → old id_token and api_key preserved, no HTTP exchange.
        assert_eq!(merged.id_token, current.id_token, "omitted id_token should be preserved");
        assert_eq!(merged.openai_api_key, current.openai_api_key, "api_key should be preserved without new id_token");
        // Email/plan/account_id also preserved without a new id_token.
        assert_eq!(merged.email, current.email);
        assert_eq!(merged.plan, current.plan);
    }

    #[tokio::test]
    async fn merge_all_omitted_preserves_everything() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: None,
            refresh_token: None,
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.id_token, current.id_token);
        assert_eq!(merged.access_token, current.access_token);
        assert_eq!(merged.refresh_token, current.refresh_token);
        assert_eq!(merged.openai_api_key, current.openai_api_key);
        assert_eq!(merged.email, current.email);
        // expires_in is None and no new access_token → old expiry preserved.
        assert_eq!(merged.expires_at, current.expires_at);
    }

    #[tokio::test]
    async fn merge_new_access_token_updates_bearer_without_id_token() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("replaced-access".to_string()),
            refresh_token: None,
            expires_in: Some(7200),
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.access_token, "replaced-access");
        // active_api_bearer_token should use the exchanged api_key (unchanged)
        // because no new id_token was provided.
        assert_eq!(active_api_bearer_token(&merged), current.openai_api_key);
    }

    // ── Blank-string refresh field tests ──
    //
    // Some token endpoints return empty strings for omitted fields rather than
    // leaving them out. These verify that blank strings are treated as omitted.

    #[tokio::test]
    async fn merge_treats_blank_access_token_as_omitted() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("   ".to_string()),
            refresh_token: Some("new-refresh".to_string()),
            expires_in: Some(3600),
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.access_token, current.access_token, "blank access_token should be treated as omitted");
        assert_eq!(merged.refresh_token, "new-refresh");
    }

    #[tokio::test]
    async fn merge_treats_blank_refresh_token_as_omitted() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("new-access".to_string()),
            refresh_token: Some(String::new()),
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.refresh_token, current.refresh_token, "blank refresh_token should be treated as omitted");
        assert_eq!(merged.access_token, "new-access");
    }

    #[tokio::test]
    async fn merge_treats_blank_id_token_as_omitted() {
        let current = sample_session();
        let resp = OpenAIRefreshResponse {
            id_token: Some("  ".to_string()),
            access_token: Some("new-access".to_string()),
            refresh_token: None,
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        // Blank id_token → treated as omitted → no HTTP exchange, old preserved.
        assert_eq!(merged.id_token, current.id_token, "blank id_token should be treated as omitted");
        assert_eq!(merged.openai_api_key, current.openai_api_key, "api_key preserved when id_token is blank");
    }

    // ── Opaque access token expiry tests ──

    #[tokio::test]
    async fn merge_changed_opaque_access_token_clears_stale_expiry() {
        let mut current = sample_session();
        current.expires_at = Some(now_secs() + 3600); // old expiry for old token

        // New access token without expires_in and without a parseable JWT exp.
        // "opaque-new-token" is not a JWT, so parse_jwt_exp returns None.
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("opaque-new-token".to_string()),
            refresh_token: None,
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.access_token, "opaque-new-token");
        // Changed token without expires_in or JWT exp → expiry must be None,
        // NOT the old token's expiry.
        assert_eq!(merged.expires_at, None, "changed opaque access token must not inherit old token's expiry");
    }

    #[tokio::test]
    async fn merge_repeated_opaque_access_token_preserves_expiry() {
        let old_expiry = now_secs() + 3600;
        let mut current = sample_session();
        current.access_token = "opaque-same-token".to_string();
        current.expires_at = Some(old_expiry);

        // The endpoint repeats the SAME opaque access token without expires_in.
        // Since the token didn't change, the old expiry is still valid.
        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: Some("opaque-same-token".to_string()),
            refresh_token: Some("new-refresh".to_string()),
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        assert_eq!(merged.access_token, "opaque-same-token");
        assert_eq!(merged.expires_at, Some(old_expiry), "repeated same opaque access token must preserve old expiry");
    }

    #[tokio::test]
    async fn merge_omitted_access_token_preserves_old_expiry() {
        let old_expiry = now_secs() + 3600;
        let mut current = sample_session();
        current.expires_at = Some(old_expiry);

        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: None,
            refresh_token: Some("new-refresh".to_string()),
            expires_in: None,
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        // No new access_token and no expires_in → old expiry preserved.
        assert_eq!(merged.expires_at, Some(old_expiry), "omitted access_token should preserve old expiry");
    }

    #[tokio::test]
    async fn merge_expires_in_overrides_for_omitted_access_token() {
        let old_expiry = now_secs() + 3600;
        let mut current = sample_session();
        current.expires_at = Some(old_expiry);

        let resp = OpenAIRefreshResponse {
            id_token: None,
            access_token: None,
            refresh_token: None,
            expires_in: Some(1800),
        };
        let merged = merge_refresh_response(&current, resp).await.expect("merge");
        // expires_in takes priority over old expiry even without a new access_token.
        assert_ne!(merged.expires_at, Some(old_expiry));
        assert!(merged.expires_at.is_some());
    }

    // ── Error classification tests (pure, no network) ──

    #[test]
    fn extract_error_code_flat_form() {
        assert_eq!(extract_error_code(r#"{"error": "invalid_grant"}"#), "invalid_grant");
    }

    #[test]
    fn extract_error_code_nested_form() {
        let body = r#"{"error": {"code": "refresh_token_expired", "message": "token expired"}}"#;
        assert_eq!(extract_error_code(body), "refresh_token_expired");
    }

    #[test]
    fn extract_error_code_nested_form_does_not_fall_back_to_message() {
        // A descriptive message that happens to contain a terminal code string
        // must NOT be treated as a structured error code. Only `code` and
        // `type` fields are recognized — `message` is free-text.
        let body = r#"{"error": {"message": "invalid_grant"}}"#;
        assert_eq!(extract_error_code(body), "", "message field should not be used as error code: {body}");
    }

    #[test]
    fn extract_error_code_empty_body() {
        assert_eq!(extract_error_code(""), "");
    }

    #[test]
    fn extract_error_code_non_json_body() {
        assert_eq!(extract_error_code("Internal Server Error"), "");
    }

    #[test]
    fn extract_error_code_blank_error_field() {
        assert_eq!(extract_error_code(r#"{"error": "  "}"#), "");
    }

    #[test]
    fn classify_refresh_status_error_invalid_grant_clears_session() {
        let _guard = TestAuthDirGuard::new();
        // Store a session so we can verify it gets cleared.
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load")
                .is_some()
        );

        let err = classify_refresh_status_error(reqwest::StatusCode::BAD_REQUEST, r#"{"error": "invalid_grant"}"#);
        assert!(err.to_string().contains("session expired"));

        // Session should have been cleared.
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn classify_refresh_status_error_nested_refresh_token_expired_clears_session() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");

        let err = classify_refresh_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": {"code": "refresh_token_expired", "message": "The refresh token has expired"}}"#,
        );
        assert!(err.to_string().contains("session expired"));

        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn classify_refresh_status_error_unauthorized_preserves_session() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");

        let err = classify_refresh_status_error(reqwest::StatusCode::UNAUTHORIZED, "");
        assert!(err.to_string().contains("HTTP 401"), "should report HTTP 401: {err}");
        // 401 without a confirmed terminal code preserves the session.
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load")
                .is_some(),
            "session should be preserved on ambiguous 401"
        );
    }

    #[test]
    fn classify_refresh_status_error_server_error_does_not_clear_session() {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");

        let err =
            classify_refresh_status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, r#"{"error": "internal_error"}"#);
        assert!(err.to_string().contains("HTTP 500"));
        // Transient error → session preserved for retry.
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load")
                .is_some()
        );
    }

    #[test]
    fn classify_refresh_status_error_never_includes_raw_body() {
        let _guard = TestAuthDirGuard::new();
        let err = classify_refresh_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "invalid_grant", "sensitive_data": "secret-leak-attempt"}"#,
        );
        assert!(!err.to_string().contains("secret-leak-attempt"), "raw body leaked into error: {err}");
    }

    // ── Table-driven classification matrix (status × body shape × expected) ──

    /// Expected outcome of `classify_refresh_status_error`.
    #[derive(Debug, PartialEq)]
    enum ClassifyOutcome {
        /// Session was cleared (terminal grant error).
        Terminal,
        /// Session was preserved; error message contains this fragment.
        Preserved(&'static str),
    }

    fn run_classify_matrix(status: reqwest::StatusCode, body: &str, expected: ClassifyOutcome) {
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");

        let err = classify_refresh_status_error(status, body);
        let msg = err.to_string();
        let loaded = load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File).expect("load");

        match expected {
            ClassifyOutcome::Terminal => {
                assert!(msg.contains("session expired"), "terminal error should say 'session expired': {msg}");
                assert!(loaded.is_none(), "session should be cleared for terminal: {status} {body}");
            }
            ClassifyOutcome::Preserved(frag) => {
                assert!(!msg.contains("session expired"), "non-terminal should not say 'session expired': {msg}");
                assert!(loaded.is_some(), "session should be preserved for: {status} {body}");
                if !frag.is_empty() {
                    assert!(msg.contains(frag), "error should contain '{frag}': {msg}");
                }
            }
        }
    }

    #[test]
    fn classify_matrix_terminal_400_flat() {
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "invalid_grant"}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_400_nested_code() {
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": {"code": "invalid_grant", "message": "..."}}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_400_nested_type() {
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": {"type": "invalid_grant", "message": "..."}}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_400_toplevel_code() {
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"code": "refresh_token_revoked", "message": "..."}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_401_flat() {
        // Terminal codes on 401 should also clear the session.
        run_classify_matrix(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error": "invalid_token"}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_401_nested() {
        run_classify_matrix(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error": {"code": "refresh_token_expired"}}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_401_refresh_token_invalidated() {
        run_classify_matrix(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error": "refresh_token_invalidated"}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_terminal_400_toplevel_refresh_token_reused() {
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"code": "refresh_token_reused", "message": "..."}"#,
            ClassifyOutcome::Terminal,
        );
    }

    #[test]
    fn classify_matrix_message_only_does_not_clear_session() {
        // A body with only a free-text "message" field (no structured code/type)
        // must NOT be treated as terminal, even if the message text happens to
        // contain a terminal code string. This prevents false-positive session
        // clearing from descriptive error messages.
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": {"message": "invalid_grant"}}"#,
            ClassifyOutcome::Preserved("HTTP 400"),
        );
    }

    #[test]
    fn classify_matrix_invalid_client_preserves() {
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "invalid_client"}"#,
            ClassifyOutcome::Preserved("invalid_client"),
        );
    }

    #[test]
    fn classify_matrix_invalid_client_401_preserves() {
        run_classify_matrix(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error": "invalid_client"}"#,
            ClassifyOutcome::Preserved("invalid_client"),
        );
    }

    #[test]
    fn classify_matrix_429_throttling_preserves() {
        run_classify_matrix(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error": "rate_limited"}"#,
            ClassifyOutcome::Preserved("rate-limited"),
        );
    }

    #[test]
    fn classify_matrix_500_server_error_preserves() {
        run_classify_matrix(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error": "internal_error"}"#,
            ClassifyOutcome::Preserved("HTTP 500"),
        );
    }

    #[test]
    fn classify_matrix_502_bad_gateway_preserves() {
        run_classify_matrix(reqwest::StatusCode::BAD_GATEWAY, "", ClassifyOutcome::Preserved("HTTP 502"));
    }

    #[test]
    fn classify_matrix_503_service_unavailable_preserves() {
        run_classify_matrix(reqwest::StatusCode::SERVICE_UNAVAILABLE, "", ClassifyOutcome::Preserved("HTTP 503"));
    }

    #[test]
    fn classify_matrix_ambiguous_401_empty_body_preserves() {
        run_classify_matrix(reqwest::StatusCode::UNAUTHORIZED, "", ClassifyOutcome::Preserved("HTTP 401"));
    }

    #[test]
    fn classify_matrix_400_nonterminal_code_preserves() {
        // A 400 with a code that is NOT in the terminal list should preserve.
        run_classify_matrix(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "some_unknown_error"}"#,
            ClassifyOutcome::Preserved("HTTP 400"),
        );
    }

    #[test]
    fn classify_matrix_never_leaks_body_in_any_branch() {
        // Terminal branch
        let _guard = TestAuthDirGuard::new();
        let err = classify_refresh_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "invalid_grant", "leak": "TERMINAL_LEAK"}"#,
        );
        assert!(!err.to_string().contains("TERMINAL_LEAK"));

        // Preserved branch (invalid_client)
        let err = classify_refresh_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "invalid_client", "leak": "CLIENT_LEAK"}"#,
        );
        assert!(!err.to_string().contains("CLIENT_LEAK"));

        // Preserved branch (429)
        let err = classify_refresh_status_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error": "rate_limited", "leak": "THROTTLE_LEAK"}"#,
        );
        assert!(!err.to_string().contains("THROTTLE_LEAK"));
    }

    // ── Error code extraction shape tests ──

    #[test]
    fn extract_error_code_nested_with_type_field() {
        let body = r#"{"error": {"type": "invalid_grant", "message": "..."}}"#;
        assert_eq!(extract_error_code(body), "invalid_grant");
    }

    #[test]
    fn extract_error_code_toplevel_code_field() {
        let body = r#"{"code": "refresh_token_expired", "message": "..."}"#;
        assert_eq!(extract_error_code(body), "refresh_token_expired");
    }

    #[test]
    fn extract_error_code_case_insensitive_normalization() {
        assert_eq!(extract_error_code(r#"{"error": "INVALID_GRANT"}"#), "invalid_grant");
    }

    #[test]
    fn extract_error_code_no_substring_matching() {
        // "invalid_grant_really" is not a terminal code — extract_error_code
        // returns it verbatim, but classify won't match it as terminal.
        let code = extract_error_code(r#"{"error": "invalid_grant_really_not_a_real_code"}"#);
        assert_eq!(code, "invalid_grant_really_not_a_real_code");
        // Verify classify does NOT treat this as terminal.
        let _guard = TestAuthDirGuard::new();
        let session = sample_session();
        save_openai_chatgpt_session_with_mode(&session, AuthCredentialsStoreMode::File).expect("save");
        let err = classify_refresh_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error": "invalid_grant_really_not_a_real_code"}"#,
        );
        assert!(!err.to_string().contains("session expired"));
        assert!(
            load_openai_chatgpt_session_with_mode(AuthCredentialsStoreMode::File)
                .expect("load")
                .is_some()
        );
    }

    // ── PKCE and callback Debug redaction tests ──

    #[test]
    fn pkce_challenge_debug_redacts_verifier() {
        let challenge = generate_pkce_challenge().expect("generate pkce");
        let debug_str = format!("{challenge:?}");
        assert!(!debug_str.contains(&challenge.code_verifier), "code_verifier leaked: {debug_str}");
        assert!(debug_str.contains("<redacted>"), "verifier should be redacted: {debug_str}");
        // Challenge and method are safe to display.
        assert!(debug_str.contains(&challenge.code_challenge), "code_challenge should be visible: {debug_str}");
    }

    #[test]
    fn auth_callback_outcome_debug_redacts_code() {
        let outcome = AuthCallbackOutcome::Code("super-secret-auth-code".to_string());
        let debug_str = format!("{outcome:?}");
        assert!(!debug_str.contains("super-secret-auth-code"), "authorization code leaked: {debug_str}");
        assert!(debug_str.contains("<redacted>"), "code should be redacted: {debug_str}");
    }

    #[test]
    fn auth_callback_outcome_debug_shows_cancelled_and_redacts_error() {
        let cancelled = format!("{:?}", AuthCallbackOutcome::Cancelled);
        assert!(cancelled.contains("Cancelled"));

        // Error messages from OAuth callbacks are untrusted query parameters
        // that may contain sensitive values — Debug must redact them.
        let error = format!("{:?}", AuthCallbackOutcome::Error("access_denied".to_string()));
        assert!(!error.contains("access_denied"), "error message leaked through Debug: {error}");
        assert!(error.contains("<redacted>"), "error should be redacted: {error}");
    }
}
