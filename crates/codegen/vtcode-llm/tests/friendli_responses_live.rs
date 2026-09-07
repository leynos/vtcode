//! Opt-in, bounded compatibility probe. No workspace tools are executed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use serde_json::json;
use vtcode_config::core::{AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig};
use vtcode_llm::provider::{LLMProvider, LLMRequest, LLMResponse, LLMStreamEvent, Message, ToolChoice, ToolDefinition};
use vtcode_llm::providers::CustomProviderBackendRouter;

const MODEL: &str = "zai-org/GLM-5.3-Flash";
const OUTPUT_CAP: u32 = 2048;
const PROBE_COUNT: u32 = 4;

fn live_provider() -> Result<CustomProviderBackendRouter> {
    ensure!(std::env::var("VTCODE_FRIENDLI_LIVE").as_deref() == Ok("1"), "explicit live opt-in is required");
    let key_path = PathBuf::from(std::env::var_os("VTCODE_FRIENDLI_LIVE_KEY_FILE").context("set key-file path")?);
    let key = std::fs::read_to_string(key_path).context("read Friendli key file")?;
    ensure!(!key.trim().is_empty(), "Friendli key file is empty");
    let config = CustomProviderConfig {
        name: "friendli-live-responses".into(),
        display_name: "Friendli bounded Responses probe".into(),
        base_url: "https://api.friendli.ai/serverless/v1".into(),
        api_format: CustomProviderApiFormat::OpenAIResponses,
        model: MODEL.into(),
        models: vec![MODEL.into()],
        supports_tools: Some(true),
        supports_reasoning: Some(true),
        supports_reasoning_effort: Some(false),
        ..Default::default()
    };
    let base_url = config.base_url.clone();
    Ok(CustomProviderBackendRouter::from_config(
        config,
        Some(key.trim().into()),
        Some(MODEL.into()),
        base_url,
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    ))
}

async fn streamed_response(provider: &CustomProviderBackendRouter, request: LLMRequest) -> Result<LLMResponse> {
    let started = Instant::now();
    let mut first_output_ms = None;
    let mut fragments = 0_usize;
    let mut stream = provider.stream(request).await?;
    while let Some(event) = stream.next().await {
        match event? {
            LLMStreamEvent::Completed { response } => {
                println!("stream first_output_ms={first_output_ms:?} fragments={fragments}");
                return Ok(*response);
            }
            LLMStreamEvent::Token { .. } | LLMStreamEvent::Reasoning { .. } => {
                let _first_output = first_output_ms.get_or_insert(started.elapsed().as_millis());
                fragments += 1;
            }
            _ => {}
        }
    }
    anyhow::bail!("Friendli stream ended without completion")
}

#[tokio::test]
#[ignore = "paid Friendli API; explicit opt-in/key-file required; at most 8192 requested output tokens"]
async fn friendli_responses_bounded_compatibility() -> Result<()> {
    ensure!(OUTPUT_CAP * PROBE_COUNT <= 10_000, "live probe output budget exceeded");
    let provider = live_provider()?;
    let cases = [
        ("buffered", "Reply with exactly OK.", None),
        ("streamed", "Reply with exactly OK.", None),
        (
            "function",
            "Call fixture_echo once with text equal to OK. Do not answer in prose.",
            Some(ToolDefinition::function(
                "fixture_echo".into(),
                "Return a synthetic test string; no external effects.".into(),
                json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}),
            )),
        ),
        (
            "custom",
            "Call fixture_raw once with the raw text OK. Do not answer in prose.",
            Some(ToolDefinition::custom(
                "fixture_raw".into(),
                "Accept the raw string OK; this is a synthetic fixture, not a real command.".into(),
            )),
        ),
    ];
    ensure!(cases.len() == usize::try_from(PROBE_COUNT)?, "probe count changed; reassess the live budget");
    for (label, prompt, tool) in cases {
        let request = LLMRequest {
            model: MODEL.into(),
            messages: vec![Message::user(prompt.into())].into(),
            tool_choice: tool.as_ref().map(|_| ToolChoice::Any),
            tools: tool.map(|tool| vec![tool].into()),
            max_tokens: Some(OUTPUT_CAP),
            stream: label != "buffered",
            ..Default::default()
        };
        let started = Instant::now();
        let response = tokio::time::timeout(Duration::from_secs(180), async {
            if label == "buffered" {
                Ok(provider.generate(request).await?)
            } else {
                streamed_response(&provider, request).await
            }
        })
        .await
        .context("live probe deadline; no automatic retry")??;
        println!(
            "{}",
            json!({"probe":label,"elapsed_ms":started.elapsed().as_millis(),"usage":response.usage,"finish_reason":format!("{:?}",response.finish_reason),"tool_count":response.tool_calls.as_ref().map_or(0,Vec::len)})
        );
        if matches!(label, "function" | "custom") {
            let calls = response.tool_calls.context("expected synthetic tool call")?;
            ensure!(calls.len() == 1, "expected exactly one synthetic tool call");
            ensure!(calls[0].is_custom() == (label == "custom"), "tool wire kind was not preserved");
        } else {
            ensure!(
                response.content.as_deref().is_some_and(|text| text.contains("OK")),
                "expected synthetic OK response"
            );
        }
    }
    Ok(())
}
