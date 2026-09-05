use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use futures::StreamExt;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde::Serialize;
use serde_json::{Value, json};

pub(crate) const MODEL: &str = "zai-org/GLM-5.3-Flash";
pub(crate) const OUTPUT_CAP: u32 = 2_048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeCase {
    BufferedText,
    StreamedText,
    Function,
    CustomRequired,
    CustomRequiredTextFormat,
    CustomNamed,
    CustomNamedTextFormat,
}

impl ProbeCase {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::BufferedText => "buffered-text",
            Self::StreamedText => "streamed-text",
            Self::Function => "function",
            Self::CustomRequired => "custom-required",
            Self::CustomRequiredTextFormat => "custom-required-text-format",
            Self::CustomNamed => "custom-named",
            Self::CustomNamedTextFormat => "custom-named-text-format",
        }
    }

    fn is_streamed(self) -> bool {
        !matches!(self, Self::BufferedText)
    }

    fn expected_tool(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Function => Some(("function_call", "fixture_echo")),
            Self::CustomRequired | Self::CustomRequiredTextFormat | Self::CustomNamed | Self::CustomNamedTextFormat => {
                Some(("custom_tool_call", "fixture_raw"))
            }
            Self::BufferedText | Self::StreamedText => None,
        }
    }

    pub(crate) fn request(self) -> Value {
        let Some((tool_type, name)) = self.expected_tool() else {
            return json!({
                "model": MODEL,
                "stream": self.is_streamed(),
                "max_output_tokens": OUTPUT_CAP,
                "input": [{"role":"user", "content":"Reply with exactly OK."}],
            });
        };
        let is_function = tool_type == "function_call";
        let content = if is_function {
            "Call fixture_echo once with text equal to OK. Do not answer in prose."
        } else {
            "Call fixture_raw once with the raw text OK. Do not answer in prose."
        };
        let tool_choice = match self {
            Self::CustomNamed | Self::CustomNamedTextFormat => json!({"type":"custom", "name":name}),
            _ => json!("required"),
        };
        let tool = if is_function {
            json!({
                "type":"function", "name":name,
                "description":"Return a synthetic test string; no external effects.",
                "parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}
            })
        } else if matches!(self, Self::CustomRequiredTextFormat | Self::CustomNamedTextFormat) {
            json!({
                "type":"custom", "name":name,
                "description":"Accept the raw string OK; this is a synthetic fixture, not a real command.",
                "format":{"type":"text"}
            })
        } else {
            json!({
                "type":"custom", "name":name,
                "description":"Accept the raw string OK; this is a synthetic fixture, not a real command."
            })
        };
        json!({
            "model": MODEL,
            "stream": self.is_streamed(),
            "max_output_tokens": OUTPUT_CAP,
            "input": [{"role":"user", "content":content}],
            "tool_choice": tool_choice,
            "tools": [tool],
        })
    }
}

impl std::str::FromStr for ProbeCase {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "buffered-text" => Ok(Self::BufferedText),
            "streamed-text" => Ok(Self::StreamedText),
            "function" => Ok(Self::Function),
            "custom-required" => Ok(Self::CustomRequired),
            "custom-required-text-format" => Ok(Self::CustomRequiredTextFormat),
            "custom-named" => Ok(Self::CustomNamed),
            "custom-named-text-format" => Ok(Self::CustomNamedTextFormat),
            unknown => bail!("unknown Friendli probe case {unknown:?}"),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum ProbeOutcome {
    TextSuccess {
        text: String,
    },
    ToolCallSuccess {
        tool_type: String,
        name: String,
        call_id: String,
        input: Value,
    },
    NoCall {
        terminal_status: String,
        text: String,
    },
    HttpFailure {
        status: u16,
        body: String,
    },
    DecodeFailure {
        error: String,
    },
    TransportFailure {
        error: String,
    },
}

impl ProbeOutcome {
    pub(crate) const fn is_success(&self) -> bool {
        matches!(self, Self::TextSuccess { .. } | Self::ToolCallSuccess { .. })
    }
}

pub(crate) fn compatibility_failure(case: ProbeCase, outcome: &ProbeOutcome) -> Option<String> {
    (!outcome.is_success()).then(|| format!("{}: {outcome:?}", case.label()))
}

#[derive(Debug)]
pub(crate) struct CapturedResponse {
    pub(crate) status: Option<u16>,
    pub(crate) safe_headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
    pub(crate) transport_error: Option<String>,
}

pub(crate) fn parse_selected_cases(raw: &str) -> Result<Vec<ProbeCase>> {
    ensure!(!raw.trim().is_empty(), "select at least one Friendli probe case");
    let cases: Vec<ProbeCase> = raw.split(',').map(str::parse).collect::<Result<_>>()?;
    ensure!(cases.len() <= 4, "at most four paid cases may be selected per invocation");
    let mut labels = cases.iter().map(|case| case.label()).collect::<Vec<_>>();
    labels.sort_unstable();
    labels.dedup();
    ensure!(labels.len() == cases.len(), "duplicate Friendli probe cases are forbidden");
    ensure!(OUTPUT_CAP * u32::try_from(cases.len())? <= 10_000, "live probe output budget exceeded");
    Ok(cases)
}

pub(crate) async fn send_case_to(
    client: &reqwest::Client,
    key: &str,
    case: ProbeCase,
    max_retries: u32,
    capture_directory: &Path,
    endpoint: &str,
) -> Result<CapturedResponse> {
    ensure!(max_retries == 0, "paid Friendli probes must not retry");
    let case_directory = capture_directory.join(case.label());
    std::fs::create_dir(&case_directory)?;
    let request = case.request();
    std::fs::write(case_directory.join("request.json"), serde_json::to_vec_pretty(&request)?)?;
    let mut response_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(case_directory.join("response.body"))?;
    let sent = client.post(endpoint).bearer_auth(key).json(&request).send().await;
    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            return Ok(CapturedResponse {
                status: None,
                safe_headers: BTreeMap::new(),
                body: Vec::new(),
                transport_error: Some(format!("send {} probe: {error}", case.label())),
            });
        }
    };
    let status = Some(response.status().as_u16());
    let safe_headers = [CONTENT_TYPE, CONTENT_LENGTH]
        .into_iter()
        .filter_map(|name| {
            response
                .headers()
                .get(&name)
                .and_then(|value| value.to_str().ok())
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .chain(response.headers().iter().filter_map(|(name, value)| {
            name.as_str()
                .starts_with("x-ratelimit-")
                .then(|| value.to_str().ok().map(|value| (name.as_str().to_string(), value.to_string())))
                .flatten()
        }))
        .collect();
    let mut body = Vec::new();
    let mut transport_error = None;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                response_file.write_all(&chunk)?;
                response_file.flush()?;
                body.extend_from_slice(&chunk);
            }
            Err(error) => {
                transport_error = Some(format!("read {} response stream: {error}", case.label()));
                break;
            }
        }
    }
    Ok(CapturedResponse { status, safe_headers, body, transport_error })
}

pub(crate) fn classify(case: ProbeCase, response: &CapturedResponse) -> ProbeOutcome {
    if let Some(error) = &response.transport_error {
        return ProbeOutcome::TransportFailure { error: error.clone() };
    }
    let Some(status) = response.status else {
        return ProbeOutcome::TransportFailure { error: "request ended without HTTP status".into() };
    };
    if !(200..300).contains(&status) {
        return ProbeOutcome::HttpFailure {
            status,
            body: std::str::from_utf8(&response.body)
                .unwrap_or("<non-UTF-8 response body>")
                .to_string(),
        };
    }
    let body = match std::str::from_utf8(&response.body) {
        Ok(body) => body,
        Err(error) => {
            return ProbeOutcome::DecodeFailure {
                error: format!("response body was not UTF-8: {error}"),
            };
        }
    };
    match decode_terminal(body) {
        Ok(terminal) => classify_terminal(case, terminal),
        Err(error) => ProbeOutcome::DecodeFailure { error: error.to_string() },
    }
}

pub(crate) fn reported_usage(response: &CapturedResponse) -> Option<Value> {
    if response.transport_error.is_some() {
        return None;
    }
    let terminal = decode_terminal(std::str::from_utf8(&response.body).ok()?).ok()?;
    let response = terminal.get("response").unwrap_or(&terminal);
    response.get("usage").cloned().filter(|usage| !usage.is_null())
}

fn decode_terminal(body: &str) -> Result<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return Ok(value);
    }
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .rev()
        .find(|event| event["type"] == "response.completed")
        .context("stream ended without response.completed")
}

fn classify_terminal(case: ProbeCase, terminal: Value) -> ProbeOutcome {
    let response = terminal.get("response").unwrap_or(&terminal);
    let status = response["status"].as_str().unwrap_or("missing").to_string();
    if status != "completed" {
        return ProbeOutcome::DecodeFailure {
            error: format!("terminal response status was {status:?}"),
        };
    }
    let outputs = response["output"].as_array().cloned().unwrap_or_default();
    if let Some((expected_type, expected_name)) = case.expected_tool() {
        let calls = outputs
            .iter()
            .filter(|item| item["type"].as_str().is_some_and(|kind| kind.ends_with("_call")))
            .collect::<Vec<_>>();
        if let [call] = calls.as_slice() {
            if call["type"] != expected_type {
                return ProbeOutcome::DecodeFailure {
                    error: format!("tool call type {:?} did not match {expected_type:?}", call["type"]),
                };
            }
            let Some(name) = call["name"].as_str() else {
                return ProbeOutcome::DecodeFailure { error: "tool call omitted name".into() };
            };
            let Some(call_id) = call["call_id"].as_str() else {
                return ProbeOutcome::DecodeFailure { error: "tool call omitted final call_id".into() };
            };
            if call_id.is_empty() {
                return ProbeOutcome::DecodeFailure {
                    error: "tool call had an empty final call_id".into(),
                };
            }
            if name != expected_name {
                return ProbeOutcome::DecodeFailure {
                    error: format!("tool call name {name:?} did not match {expected_name:?}"),
                };
            }
            let input = if expected_type == "function_call" {
                match call["arguments"].as_str().and_then(|raw| serde_json::from_str(raw).ok()) {
                    Some(value) => value,
                    None => {
                        return ProbeOutcome::DecodeFailure {
                            error: "function arguments were not strict JSON".into(),
                        };
                    }
                }
            } else {
                call.get("input").cloned().unwrap_or(Value::Null)
            };
            let valid_input = if expected_type == "function_call" {
                input == json!({"text":"OK"})
            } else {
                input == "OK"
            };
            if !valid_input {
                return ProbeOutcome::DecodeFailure { error: format!("unexpected tool input: {input}") };
            }
            return ProbeOutcome::ToolCallSuccess {
                tool_type: expected_type.into(),
                name: name.into(),
                call_id: call_id.into(),
                input,
            };
        }
        if calls.is_empty() {
            let text = output_text(&outputs);
            return ProbeOutcome::NoCall { terminal_status: status, text };
        }
        return ProbeOutcome::DecodeFailure {
            error: format!("expected exactly one tool call, received {}", calls.len()),
        };
    }
    let text = output_text(&outputs);
    if text.trim() == "OK" {
        ProbeOutcome::TextSuccess { text }
    } else {
        ProbeOutcome::DecodeFailure { error: format!("text probe omitted OK: {text:?}") }
    }
}

fn output_text(outputs: &[Value]) -> String {
    outputs
        .iter()
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}
