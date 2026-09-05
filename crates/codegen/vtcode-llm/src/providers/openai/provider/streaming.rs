//! Builds normalized OpenAI response streams and handles provider-specific stream fallbacks.
use super::super::errors::{
    fallback_model_if_not_found, format_openai_error, is_model_not_found, is_responses_api_unsupported,
};
use super::super::headers;
use super::super::responses_api::parse_responses_payload;
use super::super::stream_decoder;
use super::super::types::ResponsesApiState;
use super::OpenAIProvider;
use crate::error_display;
use crate::provider::LLMProvider;
use crate::provider::{self, LLMNormalizedStream};
use crate::providers::error_handling::{is_rate_limit_error, parse_api_error_with_headers};
use crate::providers::shared::{ResponsesNormalizedStreamOptions, create_responses_normalized_stream};
use async_stream::try_stream;
use futures::StreamExt;
use serde_json::{Value, json};

// Retained custom streaming boundary.
// Rig 0.40 exposes Responses SSE support, but this provider still needs the
// VTCode request retry/fallback path, client request id and turn metadata
// headers, ChatGPT backend defaults, prompt-cache usage parsing, and both
// LLMStreamEvent and NormalizedStreamEvent shapes. Protected by
// `api_key_responses_stream_sends_metadata_and_preserves_usage`,
// `chatgpt_responses_stream_accepts_empty_final_output_after_text_delta`, and
// the shared `responses_stream`/`stream_decoder` tests. Remove this boundary
// only once Rig can feed those exact VTCode event and error contracts.
#[inline]
fn should_prefer_responses_stream(state: ResponsesApiState) -> bool {
    !matches!(state, ResponsesApiState::Disabled)
}

enum ResponsesStreamRoute {
    Responses(reqwest::Response),
    ChatCompletions,
}

enum ResponsesStreamAttempt {
    Responses(reqwest::Response),
    RetryWithModel(String),
    ChatCompletions,
}

struct ResponsesStreamContext<'a> {
    model: &'a str,
    state: ResponsesApiState,
    request_id: &'a str,
}

fn responses_model_fallback(model: &str, status: reqwest::StatusCode, error_text: &str) -> Option<String> {
    if !is_model_not_found(status, error_text) {
        return None;
    }

    fallback_model_if_not_found(model).filter(|fallback_model| fallback_model != model)
}

fn responses_stream_error(
    provider: &OpenAIProvider,
    status: reqwest::StatusCode,
    error_text: &str,
    headers: &reqwest::header::HeaderMap,
    request_id: &str,
) -> provider::LLMError {
    if is_rate_limit_error(status.as_u16(), error_text) {
        return parse_api_error_with_headers(
            provider.error_provider_name(),
            status,
            error_text,
            headers,
            &provider.rate_limit_headers(),
        );
    }
    let formatted_error = error_display::format_llm_error(
        "OpenAI",
        &format_openai_error(status, error_text, headers, "Responses API error", Some(request_id)),
    );
    provider.provider_error_with_headers(formatted_error, status, error_text, headers)
}

impl OpenAIProvider {
    async fn send_responses_stream_request(
        &self,
        request: &provider::LLMRequest,
        request_id: &str,
    ) -> Result<reqwest::Response, provider::LLMError> {
        let mut payload = self.convert_to_openai_responses_format(request)?;
        payload["stream"] = Value::Bool(true);

        self.send_authorized(|auth| {
            headers::apply_turn_metadata(
                headers::apply_client_request_id(
                    headers::apply_responses_beta(
                        self.authorize_with_api_key(self.http_client.post(&self.responses_url[..]), auth),
                    ),
                    request_id,
                ),
                &request.metadata,
            )
            .json(&payload)
        })
        .await
    }

    async fn route_responses_stream_response(
        &self,
        response: reqwest::Response,
        context: ResponsesStreamContext<'_>,
    ) -> Result<ResponsesStreamAttempt, provider::LLMError> {
        if response.status().is_success() {
            return Ok(ResponsesStreamAttempt::Responses(response));
        }

        let status = response.status();
        let headers = response.headers().clone();
        let error_text = crate::providers::common::read_provider_error_body(response).await;

        if let Some(fallback_model) = responses_model_fallback(context.model, status, &error_text) {
            return Ok(ResponsesStreamAttempt::RetryWithModel(fallback_model));
        }

        if self.should_fallback_to_chat_completions(context.state, status, &error_text) {
            self.set_responses_api_state(context.model, ResponsesApiState::Disabled);
            return Ok(ResponsesStreamAttempt::ChatCompletions);
        }

        if is_model_not_found(status, &error_text) {
            let formatted_error = error_display::format_llm_error(
                "OpenAI",
                &format_openai_error(status, &error_text, &headers, "Model not available", Some(context.request_id)),
            );
            return Err(self.provider_error_with_headers(formatted_error, status, &error_text, &headers));
        }

        Err(responses_stream_error(self, status, &error_text, &headers, context.request_id))
    }

    fn should_fallback_to_chat_completions(
        &self,
        state: ResponsesApiState,
        status: reqwest::StatusCode,
        error_text: &str,
    ) -> bool {
        if !matches!(state, ResponsesApiState::Allowed) {
            return false;
        }
        if !is_responses_api_unsupported(status, error_text) {
            return false;
        }

        self.allows_chat_completions_fallback()
    }

    async fn request_responses_stream(
        &self,
        request: &mut provider::LLMRequest,
        responses_state: ResponsesApiState,
    ) -> Result<ResponsesStreamRoute, provider::LLMError> {
        loop {
            let request_id = Self::new_client_request_id();
            let response = self.send_responses_stream_request(request, &request_id).await?;
            match self
                .route_responses_stream_response(
                    response,
                    ResponsesStreamContext {
                        model: &request.model,
                        state: responses_state,
                        request_id: &request_id,
                    },
                )
                .await?
            {
                ResponsesStreamAttempt::RetryWithModel(model) => request.model = model,
                ResponsesStreamAttempt::Responses(response) => {
                    return Ok(ResponsesStreamRoute::Responses(response));
                }
                ResponsesStreamAttempt::ChatCompletions => return Ok(ResponsesStreamRoute::ChatCompletions),
            }
        }
    }

    pub(crate) async fn stream_normalized_request(
        &self,
        mut request: provider::LLMRequest,
    ) -> Result<LLMNormalizedStream, provider::LLMError> {
        crate::providers::common::ensure_model(&mut request, &self.model);
        if !self.supports_parallel_tool_config(&request.model) {
            request.parallel_tool_config = None;
        }

        let responses_state = self.responses_api_state(&request.model);
        if !should_prefer_responses_stream(responses_state) {
            return self.stream_chat_completions_normalized(&request).await;
        }

        match self.request_responses_stream(&mut request, responses_state).await? {
            ResponsesStreamRoute::Responses(response) => {
                let model = request.model.clone();
                let include_metrics = self.prompt_cache_enabled && self.prompt_cache_settings.surface_metrics;
                let emit_reasoning = self.model_supports_reasoning(&model);
                let parse_model = model.clone();
                Ok(create_responses_normalized_stream(
                    response,
                    ResponsesNormalizedStreamOptions {
                        provider_name: "OpenAI",
                        model: model.clone(),
                        emit_reasoning,
                        include_cached_prompt_metrics: include_metrics,
                        allow_function_call_id_remap: self.allows_responses_function_call_id_remap(&model),
                    },
                    move |value| {
                        let mut response = parse_responses_payload(value, parse_model.clone(), include_metrics)?;
                        if !emit_reasoning {
                            response.reasoning = None;
                            response.reasoning_details = None;
                        }
                        Ok(response)
                    },
                ))
            }
            ResponsesStreamRoute::ChatCompletions => self.stream_chat_completions_normalized(&request).await,
        }
    }

    pub(crate) async fn stream_request(
        &self,
        mut request: provider::LLMRequest,
    ) -> Result<provider::LLMStream, provider::LLMError> {
        crate::providers::common::ensure_model(&mut request, &self.model);
        if !self.supports_parallel_tool_config(&request.model) {
            request.parallel_tool_config = None;
        }

        let responses_state = self.responses_api_state(&request.model);

        let prefer_responses_stream = should_prefer_responses_stream(responses_state);

        if !prefer_responses_stream {
            return self.stream_chat_completions(&request).await;
        }

        match self.request_responses_stream(&mut request, responses_state).await? {
            ResponsesStreamRoute::Responses(response) => {
                let model = request.model.clone();
                let include_metrics = self.prompt_cache_enabled && self.prompt_cache_settings.surface_metrics;
                Ok(stream_decoder::create_responses_stream(
                    response,
                    self.provider_display_override.as_deref().unwrap_or("OpenAI").to_owned(),
                    model.clone(),
                    include_metrics,
                    self.model_supports_reasoning(&model),
                    self.allows_responses_function_call_id_remap(&model),
                ))
            }
            ResponsesStreamRoute::ChatCompletions => self.stream_chat_completions(&request).await,
        }
    }

    async fn stream_chat_completions(
        &self,
        request: &provider::LLMRequest,
    ) -> Result<provider::LLMStream, provider::LLMError> {
        let model = request.model.clone();
        let mut openai_request = self.convert_to_openai_format(request)?;
        openai_request["stream"] = Value::Bool(true);
        // Request usage stats in the stream (compatible with newer OpenAI models)
        // Note: Some proxies do not support stream_options and will return 400.
        if self.requests_stream_usage(&model) {
            openai_request["stream_options"] = json!({ "include_usage": true });
        }
        let url = &self.chat_completions_url[..];
        let client_request_id = Self::new_client_request_id();

        let response = self
            .send_authorized(|auth| {
                headers::apply_turn_metadata(
                    headers::apply_client_request_id(
                        self.authorize_with_api_key(self.http_client.post(url), auth),
                        &client_request_id,
                    ),
                    &request.metadata,
                )
                .json(&openai_request)
            })
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let headers = response.headers().clone();
            let error_text = crate::providers::common::read_provider_error_body(response).await;

            if is_rate_limit_error(status.as_u16(), &error_text) {
                return Err(parse_api_error_with_headers(
                    self.error_provider_name(),
                    status,
                    &error_text,
                    &headers,
                    &self.rate_limit_headers(),
                ));
            }

            let formatted_error = error_display::format_llm_error(
                "OpenAI",
                &format_openai_error(status, &error_text, &headers, "Chat Completions error", Some(&client_request_id)),
            );
            return Err(self.provider_error_with_headers(formatted_error, status, &error_text, &headers));
        }

        Ok(stream_decoder::create_chat_stream(
            response,
            self.provider_display_override.as_deref().unwrap_or("OpenAI").to_owned(),
            model.clone(),
            self.model_supports_reasoning(&model),
        ))
    }

    async fn stream_chat_completions_normalized(
        &self,
        request: &provider::LLMRequest,
    ) -> Result<LLMNormalizedStream, provider::LLMError> {
        let mut legacy_stream = self.stream_chat_completions(request).await?;
        let stream = try_stream! {
            while let Some(event) = legacy_stream.next().await {
                for normalized in event?.into_normalized() {
                    yield normalized;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::{ResponsesApiState, should_prefer_responses_stream};

    #[test]
    fn streaming_prefers_responses_for_allowed_and_required() {
        assert!(should_prefer_responses_stream(ResponsesApiState::Allowed));
        assert!(should_prefer_responses_stream(ResponsesApiState::Required));
        assert!(!should_prefer_responses_stream(ResponsesApiState::Disabled));
    }
}
