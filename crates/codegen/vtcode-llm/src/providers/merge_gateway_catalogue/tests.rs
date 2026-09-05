use super::*;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param, query_param_is_missing},
};

fn client_for(server: &MockServer) -> MergeGatewayCatalogueClient {
    let http_client = Client::builder().no_proxy().build().expect("test client should build");
    MergeGatewayCatalogueClient::try_with_client("secret-key", format!("{}/v1/openai", server.uri()), http_client)
        .expect("client should build")
}

fn catalogue_record(
    model: &str,
    provider: &str,
    display_name: Option<&str>,
    availability: &str,
    vendors: Value,
) -> Value {
    let mut record = json!({
        "availability_status": availability,
        "model": model,
        "provider": provider,
        "vendors": vendors,
    });
    if let Some(display_name) = display_name {
        record["display_name"] = json!(display_name);
    }
    record
}

fn vendor_info(
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    supports_tool_calling: bool,
    supports_structured_outputs: bool,
    streaming: bool,
    input_modalities: &[&str],
    service_tiers: &[&str],
) -> Value {
    let mut vendor = json!({
        "capabilities": {
            "input": input_modalities,
            "output": ["text"],
            "supports_tool_calling": supports_tool_calling,
            "supports_tool_choice": supports_tool_calling,
            "supports_structured_outputs": supports_structured_outputs,
            "streaming": streaming,
        },
        "pricing": {
            "currency": "USD",
            "input_per_million": 1.0,
            "output_per_million": 2.0,
        },
        "service_tiers": service_tiers,
    });
    if let Some(context_window) = context_window {
        vendor["context_window"] = json!(context_window);
    }
    if let Some(max_output_tokens) = max_output_tokens {
        vendor["max_output_tokens"] = json!(max_output_tokens);
    }
    vendor
}

fn vendor_info_with_reasoning(
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    service_tiers: &[&str],
    disable_supported: bool,
    controls: &[&str],
) -> Value {
    let mut vendor = vendor_info(context_window, max_output_tokens, true, true, true, &["text"], service_tiers);
    vendor["capabilities"]["reasoning"] = json!({
        "configurable": true,
        "disable_supported": disable_supported,
        "default_enabled": true,
        "controls": controls,
        "output_style": "summary",
    });
    vendor
}

#[tokio::test]
async fn fetch_snapshot_paginates_and_preserves_filters() {
    let server = MockServer::start().await;
    let client = client_for(&server);
    let filters = MergeCatalogueFilters {
        model: Some("anthropic/claude-opus-5".to_string()),
        provider: Some("anthropic".to_string()),
        vendor: Some("anthropic".to_string()),
    };

    let first_response = json!({
        "object": "list",
        "data": [
            catalogue_record(
                "anthropic/claude-opus-5",
                "anthropic",
                Some("Claude Opus 5"),
                "available",
                json!({
                    "anthropic": vendor_info(
                        Some(1_000_000),
                        Some(32_768),
                        true,
                        true,
                        true,
                        &["text", "image"],
                        &["standard", "flex"]
                    )
                })
            )
        ],
        "has_more": true,
        "next_cursor": "cursor-2"
    });
    let second_response = json!({
        "object": "list",
        "data": [
            catalogue_record(
                "anthropic/claude-sonnet-4.6",
                "anthropic",
                Some("Claude Sonnet 4.6"),
                "available",
                json!({
                    "anthropic": vendor_info(
                        Some(200_000),
                        Some(16_384),
                        true,
                        false,
                        true,
                        &["text", "image"],
                        &["standard"]
                    )
                })
            )
        ],
        "has_more": false
    });

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer secret-key"))
        .and(query_param_is_missing("cursor"))
        .and(query_param("model", "anthropic/claude-opus-5"))
        .and(query_param("provider", "anthropic"))
        .and(query_param("vendor", "anthropic"))
        .and(query_param("limit", "500"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(first_response)
                .insert_header("ETag", "page-1"),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer secret-key"))
        .and(query_param("model", "anthropic/claude-opus-5"))
        .and(query_param("provider", "anthropic"))
        .and(query_param("vendor", "anthropic"))
        .and(query_param("cursor", "cursor-2"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(second_response))
        .expect(1)
        .mount(&server)
        .await;

    let snapshot = client
        .fetch_snapshot(&filters, None)
        .await
        .expect("snapshot fetch should succeed")
        .expect("catalogue should be modified");

    assert_eq!(snapshot.etag.as_deref(), Some("page-1"));
    assert_eq!(snapshot.models.len(), 2);
    assert_eq!(snapshot.models[0].model, "anthropic/claude-opus-5");
    assert_eq!(
        snapshot.models[0].service_tiers,
        vec![MergeCatalogueServiceTier::Standard, MergeCatalogueServiceTier::Flex]
    );
}

#[tokio::test]
async fn fetch_snapshot_rejects_malformed_envelopes() {
    let server = MockServer::start().await;
    let client = client_for(&server);

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = client
        .fetch_snapshot(&MergeCatalogueFilters::default(), None)
        .await
        .expect_err("fetch should fail");
    assert!(error.to_string().contains("object=\"list\""));
}

#[tokio::test]
async fn fetch_snapshot_rejects_blank_next_cursors() {
    let server = MockServer::start().await;
    let client = client_for(&server);

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer secret-key"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [],
            "has_more": true,
            "next_cursor": ""
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = client
        .fetch_snapshot(&MergeCatalogueFilters::default(), None)
        .await
        .expect_err("fetch should fail");
    assert!(error.to_string().contains("next_cursor"));
}

#[tokio::test]
async fn fetch_snapshot_rejects_repeated_cursors() {
    let server = MockServer::start().await;
    let client = client_for(&server);

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param_is_missing("cursor"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [],
            "has_more": true,
            "next_cursor": "cursor-2"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("cursor", "cursor-2"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [],
            "has_more": true,
            "next_cursor": "cursor-2"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = client
        .fetch_snapshot(&MergeCatalogueFilters::default(), None)
        .await
        .expect_err("fetch should fail");
    assert!(error.to_string().contains("repeated pagination cursor"));
}

#[tokio::test]
async fn fetch_snapshot_aggregates_capabilities_conservatively() {
    let server = MockServer::start().await;
    let client = client_for(&server);

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                catalogue_record(
                    "anthropic/claude-opus-5",
                    "anthropic",
                    Some("Claude Opus 5"),
                    "available",
                    json!({
                        "anthropic": vendor_info(
                            Some(1_000_000),
                            Some(32_768),
                            true,
                            true,
                            true,
                            &["text", "image"],
                            &["standard", "flex"]
                        ),
                        "openai": vendor_info(
                            Some(128_000),
                            Some(8_192),
                            false,
                            false,
                            false,
                            &["text"],
                            &["standard", "priority"]
                        )
                    })
                )
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let snapshot = client
        .fetch_snapshot(&MergeCatalogueFilters::default(), None)
        .await
        .expect("snapshot fetch should succeed")
        .expect("catalogue should be modified");

    let model = &snapshot.models[0];
    assert_eq!(model.availability, MergeCatalogueAvailability::Available);
    assert_eq!(model.context_window, Some(128_000));
    assert_eq!(model.max_output_tokens, Some(8_192));
    assert!(!model.supports_tool_use);
    assert!(!model.supports_streaming);
    assert!(!model.supports_vision);
    assert!(!model.supports_structured_output);
    assert!(!model.supports_reasoning);
    assert!(!model.reasoning_disable_supported);
    assert!(model.reasoning_controls.is_empty());
    assert_eq!(model.service_tiers, vec![MergeCatalogueServiceTier::Standard]);
}

#[tokio::test]
async fn fetch_snapshot_aggregates_reasoning_capability() {
    let server = MockServer::start().await;
    let client = client_for(&server);

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                catalogue_record(
                    "openai/gpt-5.5",
                    "openai",
                    Some("GPT 5.5"),
                    "available",
                    json!({
                        "openai": vendor_info_with_reasoning(
                            Some(400_000),
                            Some(128_000),
                            &["standard"],
                            true,
                            &["reasoning_effort"]
                        ),
                        "azure": vendor_info_with_reasoning(
                            Some(400_000),
                            Some(128_000),
                            &["standard"],
                            true,
                            &["reasoning_effort", "reasoning_summary"]
                        )
                    })
                ),
                catalogue_record(
                    "vendor/limited",
                    "vendor",
                    None,
                    "available",
                    json!({
                        "a": vendor_info_with_reasoning(
                            Some(128_000),
                            Some(8_192),
                            &["standard"],
                            true,
                            &["thinking.budget_tokens"]
                        ),
                        "b": vendor_info(
                            Some(128_000),
                            Some(8_192),
                            true,
                            true,
                            true,
                            &["text"],
                            &["standard"]
                        )
                    })
                )
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let snapshot = client
        .fetch_snapshot(&MergeCatalogueFilters::default(), None)
        .await
        .expect("snapshot fetch should succeed")
        .expect("catalogue should be modified");

    let reasoning = &snapshot.models[0];
    assert!(reasoning.supports_reasoning);
    assert!(reasoning.reasoning_disable_supported);
    assert_eq!(reasoning.reasoning_controls, vec!["reasoning_effort", "reasoning_summary"]);

    let limited = &snapshot.models[1];
    assert!(!limited.supports_reasoning);
    assert!(!limited.reasoning_disable_supported);
    assert!(limited.reasoning_controls.is_empty());
}

#[tokio::test]
async fn fetch_snapshot_treats_304_as_not_modified_when_etag_is_supplied() {
    let server = MockServer::start().await;
    let client = client_for(&server);

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer secret-key"))
        .and(header("if-none-match", "etag-1"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;

    let snapshot = client
        .fetch_snapshot(&MergeCatalogueFilters::default(), Some("etag-1"))
        .await
        .expect("304 should not be an error");

    assert!(snapshot.is_none());
}
