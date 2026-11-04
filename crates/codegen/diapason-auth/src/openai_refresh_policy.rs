//! Pure refresh-response classification for OpenAI ChatGPT OAuth.
//!
//! This module deliberately has no storage dependency. Callers decide how to
//! apply the refresh failure action after classification.

use anyhow::anyhow;
use serde::Deserialize;

/// Side effect selected by refresh-response classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshFailureAction {
    /// The stored refresh token is unusable and should be removed.
    ClearStoredSession,
    /// Preserve credentials so a retry or configuration fix remains possible.
    PreserveStoredSession,
}

/// Classified refresh failure with a safe, user-facing error message.
#[derive(Debug)]
pub(crate) struct RefreshFailure {
    action: RefreshFailureAction,
    message: String,
}

impl RefreshFailure {
    pub(crate) fn action(&self) -> RefreshFailureAction {
        self.action
    }

    pub(crate) fn into_error(self) -> anyhow::Error {
        anyhow!("{}", self.message)
    }
}

/// Extract the OAuth 2.0 error code from a token-endpoint error body.
pub(crate) fn extract_error_code(body: &str) -> String {
    #[derive(Deserialize)]
    struct FlatErrorResponse {
        #[serde(default)]
        error: Option<serde_json::Value>,
        #[serde(default)]
        code: Option<String>,
    }

    if let Ok(parsed) = serde_json::from_str::<FlatErrorResponse>(body) {
        if let Some(serde_json::Value::String(code)) = &parsed.error
            && !code.trim().is_empty()
        {
            return code.to_ascii_lowercase();
        }

        if let Some(serde_json::Value::Object(error)) = &parsed.error {
            let code = error
                .get("code")
                .or_else(|| error.get("type"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty());
            if let Some(code) = code {
                return code.to_ascii_lowercase();
            }
        }

        if let Some(code) = &parsed.code
            && !code.trim().is_empty()
        {
            return code.to_ascii_lowercase();
        }
    }

    String::new()
}

/// Classify a non-success response without applying any storage side effect.
#[cold]
pub(crate) fn classify_refresh_failure(status: reqwest::StatusCode, body: &str) -> RefreshFailure {
    const TERMINAL_GRANT_CODES: &[&str] = &[
        "invalid_grant",
        "invalid_token",
        "refresh_token_expired",
        "refresh_token_revoked",
        "refresh_token_reused",
        "refresh_token_invalidated",
    ];

    let error_code = extract_error_code(body);
    let is_client_error = status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED;
    let is_terminal_grant = is_client_error && TERMINAL_GRANT_CODES.contains(&error_code.as_str());

    if is_terminal_grant {
        RefreshFailure {
            action: RefreshFailureAction::ClearStoredSession,
            message: "Your ChatGPT session expired. Run `diapason login openai` again.".to_string(),
        }
    } else if error_code == "invalid_client" {
        RefreshFailure {
            action: RefreshFailureAction::PreserveStoredSession,
            message: format!(
                "openai token refresh failed (HTTP {status}, invalid_client) — \
                 check your DIAPASON_OPENAI_OAUTH_CLIENT_ID / DIAPASON_OPENAI_OAUTH_ORIGINATOR configuration"
            ),
        }
    } else if status == reqwest::StatusCode::UNAUTHORIZED {
        RefreshFailure {
            action: RefreshFailureAction::PreserveStoredSession,
            message: format!(
                "openai token refresh failed (HTTP {status}) — check your OAuth client configuration and retry"
            ),
        }
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        RefreshFailure {
            action: RefreshFailureAction::PreserveStoredSession,
            message: format!("openai token refresh was rate-limited (HTTP {status}) — retry later"),
        }
    } else {
        RefreshFailure {
            action: RefreshFailureAction::PreserveStoredSession,
            message: format!("openai token refresh failed (HTTP {status})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_grant_requests_session_clear_without_echoing_body() {
        let failure = classify_refresh_failure(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","secret":"do-not-echo"}"#,
        );

        assert_eq!(failure.action(), RefreshFailureAction::ClearStoredSession);
        assert!(!failure.into_error().to_string().contains("do-not-echo"));
    }

    #[test]
    fn server_failure_preserves_session() {
        let failure = classify_refresh_failure(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "secret body");

        assert_eq!(failure.action(), RefreshFailureAction::PreserveStoredSession);
        assert_eq!(failure.into_error().to_string(), "openai token refresh failed (HTTP 500 Internal Server Error)");
    }

    #[test]
    fn error_code_matching_is_exact_and_case_insensitive() {
        assert_eq!(extract_error_code(r#"{"error":"INVALID_GRANT"}"#), "invalid_grant");
        assert_eq!(extract_error_code(r#"{"error":"invalid_grant_extra"}"#), "invalid_grant_extra");
    }
}
