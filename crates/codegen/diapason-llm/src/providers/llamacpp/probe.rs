//! HTTP model discovery and server health probing for llama.cpp.
//!
//! Extracted verbatim from the original monolithic `llamacpp.rs`. Owns the
//! `/v1/models` fetch, the `ServerProbe` result enum, and the `/health` probe
//! loop that classifies a server as ready / loading / unavailable.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::providers::common::override_base_url;
use crate::providers::ollama::base_url_to_host_root;

use diapason_config::constants::{env_vars, urls};

use super::LLAMACPP_CONNECTION_ERROR;

#[derive(Debug, Deserialize, Serialize)]
struct LlamaCppModelsResponse {
    data: Vec<LlamaCppModel>,
}

#[derive(Debug, Deserialize, Serialize)]
struct LlamaCppModel {
    id: String,
}

#[derive(Debug)]
pub(super) enum ServerProbe {
    Ready(String),
    Loading,
    Unavailable(String),
}

pub async fn fetch_llamacpp_models(base_url: Option<String>) -> Result<Vec<String>, anyhow::Error> {
    let resolved_base_url = override_base_url(urls::LLAMACPP_API_BASE, base_url, Some(env_vars::LLAMACPP_BASE_URL));
    let models_url = format!("{}/models", resolved_base_url.trim_end_matches('/'));
    let client = diapason_commons::http::create_client_with_timeout(Duration::from_secs(5));
    let response = client
        .get(&models_url)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| {
            // Connection refused is expected when llama.cpp isn't running —
            // debug-level to keep startup quiet. Error is still propagated.
            tracing::debug!("Failed to connect to llama.cpp server: {e:?}");
            anyhow::anyhow!(LLAMACPP_CONNECTION_ERROR)
        })?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to fetch llama.cpp models: HTTP {}. {}",
            response.status(),
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                "Ensure llama-server is running and exposing the OpenAI-compatible /v1 API."
            } else {
                ""
            }
        ));
    }

    let models_response: LlamaCppModelsResponse = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse llama.cpp models response: {e}"))?;

    Ok(models_response.data.into_iter().map(|model| model.id).collect())
}

pub(super) async fn probe_server(base_url: &str) -> ServerProbe {
    let host_root = base_url_to_host_root(base_url);
    let health_url = format!("{}/health", host_root.trim_end_matches('/'));
    let client = diapason_commons::http::create_client_with_timeout(Duration::from_secs(5));

    let response = match client.get(&health_url).send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!("llama.cpp health probe failed for {health_url}: {error}");
            return ServerProbe::Unavailable(LLAMACPP_CONNECTION_ERROR.to_string());
        }
    };

    if response.status().is_success() {
        return probe_reported_model(base_url).await;
    }

    let status = response.status();
    let body = crate::providers::common::read_provider_error_body(response).await;
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && body.to_ascii_lowercase().contains("loading") {
        return ServerProbe::Loading;
    }

    if status == reqwest::StatusCode::NOT_FOUND {
        return probe_reported_model(base_url).await;
    }

    ServerProbe::Unavailable(format!(
        "llama.cpp health check failed with HTTP {}{}",
        status,
        if body.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", body.trim())
        }
    ))
}

/// Fetch `/v1/models` and classify the result. Extracted from the two
/// duplicated inline blocks in `probe_server` (HTTP 200 success path and the
/// 404 fallback path), which were byte-identical aside from their call site.
async fn probe_reported_model(base_url: &str) -> ServerProbe {
    match fetch_llamacpp_models(Some(base_url.to_string())).await {
        Ok(models) if !models.is_empty() => ServerProbe::Ready(models[0].clone()),
        Ok(_) => ServerProbe::Unavailable(
            "llama.cpp is running but did not report any loaded models from /v1/models.".to_string(),
        ),
        Err(error) => ServerProbe::Unavailable(error.to_string()),
    }
}
