//! Import ChatGPT OAuth credentials from the Codex CLI's `~/.codex/auth.json`.
//!
//! This implements the "reuse Codex auth.json" integration path described in
//! the DeepWiki analysis of `openai/codex`: a third-party coding harness can
//! read the OAuth tokens that `codex login` persisted and use them directly,
//! avoiding a separate browser OAuth dance.
//!
//! ## How it works
//!
//! 1. `codex login` stores ChatGPT OAuth tokens (`id_token`, `access_token`,
//!    `refresh_token`) and a derived `OPENAI_API_KEY` in
//!    `$CODEX_HOME/auth.json` (`~/.codex/auth.json` by default).
//! 2. VT Code reads that file, parses the token data, and converts it into an
//!    [`OpenAIChatGptSession`] that the OpenAI provider can use directly.
//! 3. VT Code deliberately does **not** rotate Codex-owned refresh tokens.
//!    Copying or redeeming them could race Codex's refresh cycle or invalidate
//!    Codex-maintained credentials. Instead, [`CodexAuthJsonRefresher`]
//!    re-reads the auth.json file — which Codex refreshes independently — to
//!    obtain fresh tokens when VT Code's session needs a refresh.
//!
//! Based on patterns from [openai/codex] (Apache-2.0). Copyright 2025 OpenAI.
//! See the repository `THIRD-PARTY-NOTICES` file for full attribution.
//!
//! [openai/codex]: https://github.com/openai/codex

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::openai_chatgpt_oauth::{
    OpenAIChatGptSession, OpenAIChatGptSessionRefresher, parse_jwt_claims, parse_jwt_exp,
};

/// Codex's `~/.codex/auth.json` structure (subset relevant to ChatGPT auth).
///
/// Mirrors `AuthDotJson` from `openai/codex` `codex-rs/login/src/auth/storage.rs`.
/// Crate-private to avoid accidental token leakage through `Debug`.
#[derive(Clone, Deserialize)]
pub(crate) struct CodexAuthDotJson {
    #[serde(default)]
    pub auth_mode: Option<String>,
    /// Derived API-key-style bearer token (serde-renamed to match Codex's file).
    #[serde(rename = "OPENAI_API_KEY", default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub tokens: Option<CodexTokenData>,
    /// ISO-8601 timestamp of the last token refresh (stored as a string by Codex).
    #[serde(default)]
    pub last_refresh: Option<String>,
    /// Personal Access Token — presence (not value) indicates PAT mode.
    /// Deserialized as a redacted sentinel to avoid storing the token value.
    #[serde(default)]
    pub personal_access_token: Option<RedactedPresence>,
    /// Bedrock API key — presence (not value) indicates Bedrock mode.
    #[serde(default)]
    pub bedrock_api_key: Option<RedactedPresence>,
}

/// A deserialized value that only records whether the field was present,
/// never the actual value. Used for PAT/Bedrock credentials in Codex's
/// auth.json — we only need to know they exist for mode inference.
#[derive(Clone)]
pub(crate) struct RedactedPresence {
    _present: bool,
}

impl RedactedPresence {
    fn is_present(&self) -> bool {
        self._present
    }
}

impl<'de> Deserialize<'de> for RedactedPresence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Consume the value but discard it — we only care about presence.
        let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(RedactedPresence { _present: true })
    }
}

impl std::fmt::Debug for CodexAuthDotJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexAuthDotJson")
            .field("auth_mode", &self.auth_mode)
            .field("openai_api_key", &self.openai_api_key.as_ref().map(|_| "<redacted>"))
            .field("tokens", &self.tokens.as_ref().map(|_| "<redacted>"))
            .field("last_refresh", &self.last_refresh)
            .field("personal_access_token", &self.personal_access_token.as_ref().map(|_| "<present>"))
            .field("bedrock_api_key", &self.bedrock_api_key.as_ref().map(|_| "<present>"))
            .finish()
    }
}

/// OAuth token data stored in Codex's auth.json.
/// Crate-private to avoid accidental token leakage through `Debug`.
#[derive(Clone, Deserialize)]
pub(crate) struct CodexTokenData {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub account_id: Option<String>,
}

impl std::fmt::Debug for CodexTokenData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexTokenData")
            .field("id_token", &"<redacted>")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("account_id", &self.account_id)
            .finish()
    }
}

/// Resolve the Codex home directory (`CODEX_HOME` env var or `~/.codex`).
pub fn codex_home_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("CODEX_HOME")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    dirs::home_dir()
        .map(|home| home.join(".codex"))
        .ok_or_else(|| anyhow!("could not determine home directory for codex auth path"))
}

/// Path to Codex's `auth.json`.
pub fn codex_auth_json_path() -> Result<PathBuf> {
    Ok(codex_home_dir()?.join("auth.json"))
}

/// Check whether Codex's `auth.json` exists on disk.
pub fn codex_auth_json_exists() -> bool {
    codex_auth_json_path().map(|path| path.exists()).unwrap_or(false)
}

/// Read and parse Codex's `auth.json`.
///
/// Crate-private: the error messages intentionally omit the absolute path to
/// avoid leaking the user's home directory in logs or CLI output.
pub(crate) fn read_codex_auth_json() -> Result<CodexAuthDotJson> {
    let path = codex_auth_json_path()?;
    let data = std::fs::read(&path).with_context(|| "failed to read codex auth.json")?;
    serde_json::from_slice::<CodexAuthDotJson>(&data).with_context(|| "failed to parse codex auth.json")
}

/// Try to load a ChatGPT session from Codex's `auth.json` in a single attempt.
///
/// This is the retry-free primitive shared by the synchronous loader and the
/// async [`CodexAuthJsonRefresher`]. It reads the file directly (no
/// exists-precheck) so there is no TOCTOU window between stat and read.
///
/// Returns `Ok(Some(session))` if Codex has ChatGPT OAuth tokens, `Ok(None)` if
/// the file does not exist, contains no token data, or is configured for a
/// non-ChatGPT auth mode (e.g. API key). Returns `Err` if the file exists but
/// cannot be read or parsed (transient I/O or partial-write conditions).
///
/// **Mode inference** mirrors Codex's own `resolved_mode` precedence:
/// explicit `auth_mode` → `personal_access_token` → `bedrock_api_key` →
/// `OPENAI_API_KEY` → ChatGPT (legacy default when none of the above are
/// present). When `auth_mode` is absent, the presence of PAT, Bedrock, or
/// OPENAI_API_KEY (even empty) selects a non-ChatGPT mode.
fn try_load_codex_chatgpt_session_once() -> Result<Option<OpenAIChatGptSession>> {
    let path = codex_auth_json_path()?;
    // Read directly — no exists() precheck — to avoid a TOCTOU race where the
    // file appears, then is truncated by a concurrent Codex refresh between the
    // stat and the read. Map NotFound to Ok(None); all other errors propagate.
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(anyhow!("failed to read codex auth.json")),
    };
    let auth = serde_json::from_slice::<CodexAuthDotJson>(&data).with_context(|| "failed to parse codex auth.json")?;
    // Reject explicitly non-ChatGPT auth modes (e.g. "apikey").
    if let Some(mode) = auth.auth_mode.as_deref()
        && !mode.eq_ignore_ascii_case("chatgpt")
    {
        return Ok(None);
    }
    // When auth_mode is absent, check Codex's resolved_mode precedence:
    // PAT → Bedrock → OPENAI_API_KEY → ChatGPT. Presence of any of the
    // non-ChatGPT credentials (even empty values) selects that mode.
    if auth.auth_mode.is_none() {
        if auth.personal_access_token.as_ref().is_some_and(|p| p.is_present()) {
            return Ok(None);
        }
        if auth.bedrock_api_key.as_ref().is_some_and(|p| p.is_present()) {
            return Ok(None);
        }
        if auth.openai_api_key.is_some() {
            return Ok(None);
        }
    }
    let Some(tokens) = &auth.tokens else {
        return Ok(None);
    };
    // Require a nonblank access_token — it is the primary bearer credential.
    if tokens.access_token.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(codex_tokens_to_session(&auth, tokens)))
}

/// Try to load a ChatGPT session from Codex's `auth.json`.
///
/// Wraps the retry-free loader with a small bounded
/// synchronous retry loop. Codex writes `auth.json` by truncating and
/// rewriting without an atomic rename, so a concurrent Codex refresh can make
/// the file momentarily empty or partial. The retry absorbs that transient
/// window without sleeping when the file is absent (`Ok(None)`) or already
/// successfully parsed (`Ok(Some(_))`).
///
/// Returns `Ok(Some(session))` if Codex has ChatGPT OAuth tokens, `Ok(None)` if
/// the file does not exist, contains no token data, or is configured for a
/// non-ChatGPT auth mode (e.g. API key). Returns `Err` if the file exists but
/// cannot be read or parsed after all retries.
pub fn try_load_codex_chatgpt_session() -> Result<Option<OpenAIChatGptSession>> {
    let mut last_err = None;
    for attempt in 0..CODEX_REFRESH_MAX_ATTEMPTS {
        match try_load_codex_chatgpt_session_once() {
            Ok(result) => return Ok(result),
            Err(err) => last_err = Some(err),
        }
        // Only sleep between attempts, not after the last one.
        if attempt + 1 < CODEX_REFRESH_MAX_ATTEMPTS {
            if let Some(&delay) = CODEX_REFRESH_BACKOFF_MS.get(attempt) {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("failed to read codex auth.json after retries")))
}

/// Convert Codex auth tokens into a VT Code [`OpenAIChatGptSession`].
///
/// The Codex `refresh_token` is deliberately NOT copied into the session.
/// Codex-owned tokens are not rotated by VT Code — copying or redeeming them
/// could race Codex's refresh cycle or invalidate Codex-maintained credentials.
/// The external refresher (`CodexAuthJsonRefresher`) re-reads `auth.json`
/// instead. Storing an empty string prevents the Codex refresh token from
/// lingering in memory or leaking through the session's `Debug` output.
fn codex_tokens_to_session(auth: &CodexAuthDotJson, tokens: &CodexTokenData) -> OpenAIChatGptSession {
    let now = now_secs();
    // Parse JWT claims for email / account_id / plan (same logic as the OAuth flow).
    let claims = parse_jwt_claims(&tokens.id_token).ok();
    // Extract the `exp` claim from the access_token JWT (Codex doesn't store
    // expiry separately). This lets the session detect expired tokens and
    // trigger a refresh (reread of auth.json) rather than sending stale creds.
    let expires_at = parse_jwt_exp(&tokens.access_token);
    OpenAIChatGptSession {
        // Use Codex's derived API key if present; the provider falls back to the
        // OAuth access_token when this field is empty.
        openai_api_key: auth.openai_api_key.clone().unwrap_or_default(),
        id_token: tokens.id_token.clone(),
        access_token: tokens.access_token.clone(),
        // Deliberately empty — Codex-owned refresh tokens are not copied or
        // rotated by VT Code (ownership/race-avoidance, see module docs).
        refresh_token: String::new(),
        account_id: tokens
            .account_id
            .clone()
            .or_else(|| claims.as_ref().and_then(|c| c.account_id.clone())),
        email: claims.as_ref().and_then(|c| c.email.clone()),
        plan: claims.as_ref().and_then(|c| c.plan.clone()),
        // Treat the imported session as freshly obtained so it is not
        // immediately refreshed (the tokens are valid bearer tokens).
        obtained_at: now,
        refreshed_at: now,
        expires_at,
    }
}

/// Check whether a session's access token has expired.
///
/// Uses `expires_at` (from the JWT `exp` claim) with a safety skew. Returns
/// `false` when `expires_at` is `None` (expiry unknown — assume valid).
pub(crate) fn is_session_expired(session: &OpenAIChatGptSession) -> bool {
    let Some(expires_at) = session.expires_at else {
        return false;
    };
    now_secs().saturating_add(60) >= expires_at
}

/// A session refresher that re-reads Codex's `auth.json` to obtain fresh tokens.
///
/// Codex-owned refresh tokens are not rotated by VT Code — copying or
/// redeeming them could race Codex's refresh cycle or invalidate
/// Codex-maintained credentials. Instead, this refresher relies on Codex
/// refreshing its auth.json independently (e.g. when Codex is running) and
/// re-reads the file.
///
/// Codex writes `auth.json` by truncating and rewriting without an atomic
/// rename, so a single read can observe an empty or partial file during a
/// concurrent Codex refresh. The refresher retries a few times with short
/// backoff before giving up, and only replaces the in-memory session after a
/// complete, valid parse.
pub struct CodexAuthJsonRefresher;

const CODEX_REFRESH_MAX_ATTEMPTS: usize = 4;
const CODEX_REFRESH_BACKOFF_MS: &[u64] = &[10, 30, 100];

#[async_trait]
impl OpenAIChatGptSessionRefresher for CodexAuthJsonRefresher {
    async fn refresh_session(&self, current: &OpenAIChatGptSession) -> Result<OpenAIChatGptSession> {
        let mut last_err = None;
        for attempt in 0..CODEX_REFRESH_MAX_ATTEMPTS {
            // Use the retry-free primitive — this fn owns the async retry loop.
            // Calling the public try_load_codex_chatgpt_session() here would
            // multiply delays (sync retries inside async retries).
            match try_load_codex_chatgpt_session_once() {
                Ok(Some(session)) => {
                    // Reject any known-expired token. If it's the same as the
                    // current token, Codex hasn't refreshed the file. If it's
                    // different but also expired, it's a rotated-but-stale
                    // replacement — still not usable.
                    if is_session_expired(&session) {
                        if session.access_token == current.access_token {
                            bail!(
                                "Codex's auth.json contains an expired access token that has not been refreshed. \
                                 Run `codex login` to refresh it, or `vtcode login openai` for a VT Code session."
                            );
                        }
                        bail!(
                            "Codex's auth.json contains an expired replacement access token. \
                             Run `codex login` to refresh it, or `vtcode login openai` for a VT Code session."
                        );
                    }
                    return Ok(session);
                }
                Ok(None) => {
                    // File missing, no tokens, or non-ChatGPT mode — no point retrying.
                    bail!(
                        "Codex auth.json no longer contains ChatGPT tokens. \
                         Run `codex login` to refresh it, or `vtcode login openai` for a VT Code session."
                    );
                }
                Err(err) => {
                    // Transient I/O or parse failure — likely a partial write.
                    last_err = Some(err);
                    if attempt + 1 < CODEX_REFRESH_MAX_ATTEMPTS {
                        let delay = CODEX_REFRESH_BACKOFF_MS.get(attempt).copied().unwrap_or(100);
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
            }
        }
        // All retries exhausted — propagate the last transient error.
        Err(last_err.unwrap_or_else(|| anyhow!("failed to read codex auth.json after retries")))
    }
}

/// Create a shared [`CodexAuthJsonRefresher`] handle for use with the external
/// constructor on [`crate::OpenAIChatGptAuthHandle`].
pub fn codex_auth_json_refresher() -> Arc<dyn OpenAIChatGptSessionRefresher> {
    Arc::new(CodexAuthJsonRefresher)
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
    use serial_test::serial;

    #[test]
    fn parse_codex_auth_json_with_tokens() {
        let json = r#"{
            "OPENAI_API_KEY": "sk-derived-key",
            "tokens": {
                "id_token": "header.eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjLTEyMyIsImNoYXRncHRfcGxhbl90eXBlIjoicGx1cyJ9fQ.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh",
                "account_id": "acc-123"
            },
            "last_refresh": "2025-01-15T12:00:00Z"
        }"#;
        let auth: CodexAuthDotJson = serde_json::from_str(json).expect("parse");
        assert_eq!(auth.openai_api_key.as_deref(), Some("sk-derived-key"));
        let tokens = auth.tokens.expect("tokens present");
        assert_eq!(tokens.access_token, "oauth-access");
        assert_eq!(tokens.refresh_token, "oauth-refresh");
        assert_eq!(tokens.account_id.as_deref(), Some("acc-123"));
    }

    #[test]
    fn parse_codex_auth_json_without_tokens() {
        let json = r#"{"OPENAI_API_KEY": "sk-key"}"#;
        let auth: CodexAuthDotJson = serde_json::from_str(json).expect("parse");
        assert!(auth.tokens.is_none());
    }

    #[test]
    fn codex_tokens_to_session_maps_fields() {
        let auth = CodexAuthDotJson {
            auth_mode: Some("chatgpt".to_string()),
            openai_api_key: Some("sk-derived".to_string()),
            tokens: Some(CodexTokenData {
                id_token: "header.eyJlbWFpbCI6InVzZXJAdGVzdC5jb20ifQ.sig".to_string(),
                access_token: "access-tok".to_string(),
                refresh_token: "refresh-tok".to_string(),
                account_id: Some("acc-456".to_string()),
            }),
            last_refresh: None,
            personal_access_token: None,
            bedrock_api_key: None,
        };
        let tokens = auth.tokens.clone().unwrap();
        let session = codex_tokens_to_session(&auth, &tokens);
        assert_eq!(session.openai_api_key, "sk-derived");
        assert_eq!(session.access_token, "access-tok");
        // refresh_token is deliberately empty — Codex-owned refresh tokens
        // are not copied into the VTCode session (ownership/race-avoidance).
        assert_eq!(session.refresh_token, "");
        assert_eq!(session.account_id.as_deref(), Some("acc-456"));
        // email is parsed from the JWT payload
        assert_eq!(session.email.as_deref(), Some("user@test.com"));
    }

    #[test]
    fn codex_tokens_to_session_falls_back_to_empty_api_key() {
        let auth = CodexAuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(CodexTokenData {
                id_token: "header.e30.sig".to_string(),
                access_token: "access-tok".to_string(),
                refresh_token: "refresh-tok".to_string(),
                account_id: None,
            }),
            last_refresh: None,
            personal_access_token: None,
            bedrock_api_key: None,
        };
        let tokens = auth.tokens.clone().unwrap();
        let session = codex_tokens_to_session(&auth, &tokens);
        assert!(session.openai_api_key.is_empty());
        assert!(session.account_id.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn codex_refresher_fails_when_no_auth_file() {
        // Point CODEX_HOME at an empty temp dir so the refresher cannot find auth.json.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let refresher = CodexAuthJsonRefresher;
        let session = OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: String::new(),
            access_token: String::new(),
            refresh_token: String::new(),
            account_id: None,
            email: None,
            plan: None,
            obtained_at: 0,
            refreshed_at: 0,
            expires_at: None,
        };
        let result = refresher.refresh_session(&session).await;
        // Restore the env var.
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no longer contains ChatGPT tokens"));
    }

    #[test]
    #[serial]
    fn codex_apikey_mode_is_rejected_as_chatgpt_fallback() {
        // auth_mode = "apikey" must not produce a ChatGPT session.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "auth_mode": "apikey",
            "OPENAI_API_KEY": "sk-test",
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(session.is_ok(), "parsing should not error");
        assert!(session.unwrap().is_none(), "apikey mode should not produce a ChatGPT session");
    }

    #[test]
    #[serial]
    fn codex_blank_access_token_is_rejected() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "   ",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(session.is_ok());
        assert!(session.unwrap().is_none(), "blank access_token should not produce a session");
    }

    #[test]
    #[serial]
    fn codex_chatgpt_mode_with_access_token_is_accepted() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        // Even without a derived API key, a valid access_token is sufficient.
        let auth_json = r#"{
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access-token",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let session = session.expect("parse ok").expect("session should be present");
        assert_eq!(session.access_token, "oauth-access-token");
        assert!(session.openai_api_key.is_empty(), "no derived API key in this fixture");
    }

    #[test]
    #[serial]
    fn codex_no_auth_mode_with_api_key_is_rejected() {
        // When auth_mode is absent and OPENAI_API_KEY is present (even empty),
        // Codex treats it as API-key mode — not a ChatGPT session. VT Code must
        // match this (presence-based, not blank-based).
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "OPENAI_API_KEY": "",
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(session.is_ok());
        assert!(
            session.unwrap().is_none(),
            "missing auth_mode + present (even empty) API key should be API-key mode, not ChatGPT"
        );
    }

    #[test]
    #[serial]
    fn codex_no_auth_mode_no_api_key_defaults_to_chatgpt() {
        // When auth_mode is absent AND no OPENAI_API_KEY, legacy default is ChatGPT.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(session.is_ok());
        assert!(session.unwrap().is_some(), "missing auth_mode + no API key + tokens should default to ChatGPT");
    }

    // ── PAT / Bedrock mode inference (verified against openai/codex source) ──

    #[test]
    #[serial]
    fn codex_pat_presence_suppresses_chatgpt_fallback() {
        // When auth_mode is absent and personal_access_token is present,
        // Codex's resolved_mode returns PersonalAccessToken — not ChatGPT.
        // VT Code must reject this as a ChatGPT session.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "personal_access_token": "pat-secret-value",
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(session.is_ok());
        assert!(session.unwrap().is_none(), "PAT presence (even with tokens) should suppress ChatGPT fallback");
    }

    #[test]
    #[serial]
    fn codex_bedrock_presence_suppresses_chatgpt_fallback() {
        // When auth_mode is absent and bedrock_api_key is present,
        // Codex's resolved_mode returns BedrockApiKey — not ChatGPT.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "bedrock_api_key": {"api_key": "bedrock-secret", "region": "us-east-1"},
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(session.is_ok());
        assert!(session.unwrap().is_none(), "Bedrock presence (even with tokens) should suppress ChatGPT fallback");
    }

    #[test]
    #[serial]
    fn codex_explicit_chatgpt_mode_overrides_pat_presence() {
        // Explicit auth_mode = "chatgpt" wins over PAT presence, matching
        // Codex's resolved_mode: explicit auth_mode is checked first.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let auth_json = r#"{
            "auth_mode": "chatgpt",
            "personal_access_token": "pat-should-be-ignored",
            "tokens": {
                "id_token": "header.e30.sig",
                "access_token": "oauth-access",
                "refresh_token": "oauth-refresh"
            }
        }"#;
        std::fs::write(temp.path().join("auth.json"), auth_json).expect("write auth.json");
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let session = session.expect("parse ok").expect("session should be present");
        assert_eq!(session.access_token, "oauth-access");
    }

    #[test]
    fn codex_auth_json_debug_never_leaks_token_values() {
        // The custom Debug for CodexAuthDotJson must redact all credential values.
        let auth = CodexAuthDotJson {
            auth_mode: Some("chatgpt".to_string()),
            openai_api_key: Some("sk-secret-key".to_string()),
            tokens: Some(CodexTokenData {
                id_token: "id-secret".to_string(),
                access_token: "access-secret".to_string(),
                refresh_token: "refresh-secret".to_string(),
                account_id: Some("acc-123".to_string()),
            }),
            last_refresh: None,
            personal_access_token: Some(RedactedPresence { _present: true }),
            bedrock_api_key: Some(RedactedPresence { _present: true }),
        };
        let debug_str = format!("{auth:?}");
        assert!(!debug_str.contains("sk-secret-key"), "api key leaked: {debug_str}");
        assert!(!debug_str.contains("id-secret"), "id_token leaked: {debug_str}");
        assert!(!debug_str.contains("access-secret"), "access_token leaked: {debug_str}");
        assert!(!debug_str.contains("refresh-secret"), "refresh_token leaked: {debug_str}");
        // Non-secret metadata should still be visible.
        assert!(debug_str.contains("chatgpt"), "auth_mode should be visible: {debug_str}");
        // account_id is inside the redacted tokens field, so it should NOT appear.
        assert!(!debug_str.contains("acc-123"), "account_id inside tokens should be redacted: {debug_str}");
    }

    // ── Helper: build a JWT with an `exp` claim for expiry testing ──

    fn make_jwt_with_exp(exp: u64) -> String {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = format!(r#"{{"exp":{exp}}}"#);
        let encoded = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("header.{encoded}.sig")
    }

    fn make_jwt_without_exp() -> String {
        // Opaque token with no exp claim — simulates non-JWT access tokens.
        "opaque-access-token-not-a-jwt".to_string()
    }

    fn write_codex_auth_json(dir: &std::path::Path, json: &str) {
        std::fs::write(dir.join("auth.json"), json).expect("write auth.json");
    }

    fn codex_auth_json_with_access_token(access_token: &str) -> String {
        format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"id_token":"header.e30.sig","access_token":"{access_token}","refresh_token":"oauth-refresh"}}}}"#
        )
    }

    // ── Initial-load retry tests ──

    #[test]
    #[serial]
    fn initial_load_returns_ok_none_when_file_absent() {
        // No file at all — should return Ok(None) immediately, no error.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let result = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(result.is_ok(), "missing file should be Ok(None), not Err");
        assert!(result.unwrap().is_none());
    }

    #[test]
    #[serial]
    fn initial_load_retries_partial_write_then_succeeds() {
        // Simulate a concurrent Codex refresh: file starts as truncated/invalid
        // JSON, then becomes valid. The bounded retry should absorb the partial
        // write and return the session once the file is complete.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        // Write a truncated file (partial write — invalid JSON).
        write_codex_auth_json(temp.path(), r#"{"auth_mode":"chatg"#);
        // Spawn a thread that completes the file after a short delay.
        let path = temp.path().join("auth.json");
        drop(std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(15));
            std::fs::write(&path, codex_auth_json_with_access_token("oauth-access")).expect("complete the file");
        }));
        let result = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let session = result.expect("should succeed after retry");
        assert!(session.is_some(), "retry should absorb the partial write and return the session");
        assert_eq!(session.unwrap().access_token, "oauth-access");
    }

    #[test]
    #[serial]
    fn initial_load_returns_path_neutral_error_after_retries_exhausted() {
        // File is permanently invalid JSON — retries should exhaust and return
        // a path-neutral parse error (no absolute home path in the message).
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        write_codex_auth_json(temp.path(), "this is not json at all {{{{");
        let result = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let err = result.expect_err("permanently invalid JSON should error");
        let msg = err.to_string();
        assert!(msg.contains("failed to parse codex auth.json"), "error should mention parse failure: {msg}");
        // Path-neutral: the error must not contain the temp dir path or "codex" path.
        assert!(!msg.contains(temp.path().to_str().unwrap()), "error must not leak absolute path: {msg}");
    }

    // ── Refresher expiry tests ──

    #[tokio::test]
    #[serial]
    async fn refresher_rejects_changed_but_expired_access_token() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        // Current session has an expired token.
        let expired_jwt = make_jwt_with_exp(now_secs().saturating_sub(3600));
        let current = OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: String::new(),
            access_token: expired_jwt.clone(),
            refresh_token: String::new(),
            account_id: None,
            email: None,
            plan: None,
            obtained_at: 0,
            refreshed_at: 0,
            expires_at: Some(now_secs().saturating_sub(3600)),
        };
        // Codex "refreshed" the file with a different but also-expired token.
        let new_expired_jwt = make_jwt_with_exp(now_secs().saturating_sub(1800));
        write_codex_auth_json(temp.path(), &codex_auth_json_with_access_token(&new_expired_jwt));
        let refresher = CodexAuthJsonRefresher;
        let result = refresher.refresh_session(&current).await;
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let err = result.expect_err("expired replacement should be rejected");
        assert!(
            err.to_string().contains("expired replacement access token"),
            "should mention expired replacement: {err}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn refresher_accepts_changed_valid_access_token() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        // Current session has an expired token.
        let expired_jwt = make_jwt_with_exp(now_secs().saturating_sub(3600));
        let current = OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: String::new(),
            access_token: expired_jwt,
            refresh_token: String::new(),
            account_id: None,
            email: None,
            plan: None,
            obtained_at: 0,
            refreshed_at: 0,
            expires_at: Some(now_secs().saturating_sub(3600)),
        };
        // Codex refreshed the file with a new valid (future-expiry) token.
        let valid_jwt = make_jwt_with_exp(now_secs().saturating_add(3600));
        write_codex_auth_json(temp.path(), &codex_auth_json_with_access_token(&valid_jwt));
        let refresher = CodexAuthJsonRefresher;
        let result = refresher.refresh_session(&current).await;
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let session = result.expect("valid replacement should be accepted");
        assert!(!session.access_token.is_empty(), "should return the refreshed session");
    }

    #[tokio::test]
    #[serial]
    async fn refresher_rejects_unchanged_expired_access_token() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let expired_jwt = make_jwt_with_exp(now_secs().saturating_sub(3600));
        let current = OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: String::new(),
            access_token: expired_jwt.clone(),
            refresh_token: String::new(),
            account_id: None,
            email: None,
            plan: None,
            obtained_at: 0,
            refreshed_at: 0,
            expires_at: Some(now_secs().saturating_sub(3600)),
        };
        // File has the SAME expired token — Codex hasn't refreshed it.
        write_codex_auth_json(temp.path(), &codex_auth_json_with_access_token(&expired_jwt));
        let refresher = CodexAuthJsonRefresher;
        let result = refresher.refresh_session(&current).await;
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let err = result.expect_err("unchanged expired token should be rejected");
        assert!(err.to_string().contains("has not been refreshed"), "should mention Codex hasn't refreshed: {err}");
    }

    // ── Unknown-expiry behaviour ──

    #[test]
    #[serial]
    fn unknown_expiry_session_is_treated_as_valid() {
        // An opaque (non-JWT) access token has no exp claim → expires_at = None.
        // The session should still load and is_session_expired should return false.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let opaque_token = make_jwt_without_exp();
        write_codex_auth_json(temp.path(), &codex_auth_json_with_access_token(&opaque_token));
        let session = try_load_codex_chatgpt_session();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let session = session.expect("parse ok").expect("session present");
        assert!(session.expires_at.is_none(), "opaque token should have no expiry");
        assert!(!is_session_expired(&session), "unknown expiry should be treated as valid (not expired)");
    }

    #[tokio::test]
    #[serial]
    async fn refresher_accepts_unknown_expiry_token() {
        // An opaque token with no exp claim should be accepted by the refresher
        // (unknown expiry is treated as valid, matching the session-expiry policy).
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        let opaque_token = make_jwt_without_exp();
        write_codex_auth_json(temp.path(), &codex_auth_json_with_access_token(&opaque_token));
        let current = OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: String::new(),
            access_token: "previous-opaque".to_string(),
            refresh_token: String::new(),
            account_id: None,
            email: None,
            plan: None,
            obtained_at: 0,
            refreshed_at: 0,
            expires_at: None,
        };
        let refresher = CodexAuthJsonRefresher;
        let result = refresher.refresh_session(&current).await;
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        let session = result.expect("unknown-expiry token should be accepted");
        assert_eq!(session.access_token, opaque_token);
    }

    // ── Refresher does not multiply retry loops ──

    #[tokio::test]
    #[serial]
    async fn refresher_uses_single_attempt_primitive() {
        // If the refresher called the public try_load_codex_chatgpt_session()
        // (which has its own sync retry loop), a permanently-invalid file would
        // incur sync sleeps INSIDE each async attempt, multiplying delays.
        // This test verifies the refresher completes quickly even with a
        // permanently invalid file, proving it uses the single-attempt primitive.
        let temp = tempfile::tempdir().expect("create temp dir");
        let prev = std::env::var("CODEX_HOME").ok();
        vtcode_commons::env_lock::set_var("CODEX_HOME", temp.path());
        write_codex_auth_json(temp.path(), "invalid json {{{{");
        let current = OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: String::new(),
            access_token: "old".to_string(),
            refresh_token: String::new(),
            account_id: None,
            email: None,
            plan: None,
            obtained_at: 0,
            refreshed_at: 0,
            expires_at: None,
        };
        let refresher = CodexAuthJsonRefresher;
        let start = std::time::Instant::now();
        let result = refresher.refresh_session(&current).await;
        let elapsed = start.elapsed();
        vtcode_commons::env_lock::lock().restore_var("CODEX_HOME", prev.as_deref());
        assert!(result.is_err(), "invalid file should error");
        // The async refresher sleeps 10+30+100=140ms across its 4 attempts.
        // If it also called the sync retry wrapper (which sleeps the same),
        // total would be ~280ms+. We allow 200ms as a generous upper bound
        // that proves no double-retry.
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "refresher should not multiply retry delays (took {elapsed:?})"
        );
    }
}
