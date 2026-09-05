//! Offline validation for bounded Friendli Responses probe capture and classification.

mod support;

use std::collections::BTreeMap;

use anyhow::Context;
use proptest::prelude::*;
use serde_json::Value;
use support::friendli_probe::{
    CapturedResponse, OUTPUT_CAP, ProbeCase, ProbeOutcome, classify, compatibility_failure, parse_selected_cases,
    reported_usage, send_case_to,
};

const FIXTURES: &str = "../../../tests/fixtures/friendli/issue36";

fn captured_response(stem: &str, status: u16, extension: &str) -> anyhow::Result<CapturedResponse> {
    let path = format!("{}/{FIXTURES}/{stem}-response.{extension}", env!("CARGO_MANIFEST_DIR"));
    Ok(CapturedResponse {
        status: Some(status),
        safe_headers: BTreeMap::new(),
        body: std::fs::read(&path).with_context(|| format!("read sanitized Friendli response fixture {path}"))?,
        transport_error: None,
    })
}

fn completed_response(output: Value) -> CapturedResponse {
    response_with_status("completed", output)
}

fn response_with_status(status: &str, output: Value) -> CapturedResponse {
    CapturedResponse {
        status: Some(200),
        safe_headers: BTreeMap::new(),
        body: format!(
            "data: {}\n\n",
            serde_json::json!({"type":"response.completed","response":{"status":status,"output":output}})
        )
        .into_bytes(),
        transport_error: None,
    }
}

#[test]
fn captured_function_response_has_strict_input_and_retains_final_identity() -> anyhow::Result<()> {
    let response = captured_response("function", 200, "sse")?;
    let ProbeOutcome::ToolCallSuccess { name, call_id, input, .. } = classify(ProbeCase::Function, &response) else {
        panic!("captured function response must classify as one valid call");
    };
    assert_eq!(name, "fixture_echo");
    assert_eq!(call_id, "chatcmpl-tool-aec11d97912311ee");
    assert_eq!(input, serde_json::json!({"text":"OK"}));
    let usage = reported_usage(&response).context("captured function response omitted usage")?;
    assert_eq!(usage["total_tokens"], 224);
    Ok(())
}

#[test]
fn changed_function_identity_or_input_is_rejected() {
    for call in [
        serde_json::json!({"type":"function_call","name":"changed","call_id":"final-call","arguments":"{\"text\":\"OK\"}"}),
        serde_json::json!({"type":"function_call","name":"fixture_echo","call_id":"final-call","arguments":"{text:OK}"}),
        serde_json::json!({"type":"function_call","name":"fixture_echo","call_id":"","arguments":"{\"text\":\"OK\"}"}),
    ] {
        let response = CapturedResponse {
            status: Some(200),
            safe_headers: BTreeMap::new(),
            body: format!(
                "data: {}\n\n",
                serde_json::json!({"type":"response.completed","response":{"status":"completed","output":[call]}})
            )
            .into_bytes(),
            transport_error: None,
        };
        assert!(matches!(classify(ProbeCase::Function, &response), ProbeOutcome::DecodeFailure { .. }));
    }
}

#[test]
fn multiple_tool_calls_are_not_misreported_as_no_call() {
    let response = completed_response(serde_json::json!([
        {"type":"custom_tool_call","name":"fixture_raw","call_id":"one","input":"OK"},
        {"type":"custom_tool_call","name":"fixture_raw","call_id":"two","input":"OK"}
    ]));

    assert!(matches!(classify(ProbeCase::CustomNamed, &response), ProbeOutcome::DecodeFailure { .. }));
}

#[test]
fn text_probe_requires_exact_trimmed_ok() {
    let accepted =
        completed_response(serde_json::json!([{"type":"message","content":[{"type":"output_text","text":"  OK\n"}]}]));
    let rejected =
        completed_response(serde_json::json!([{"type":"message","content":[{"type":"output_text","text":"NOT OK"}]}]));

    assert!(classify(ProbeCase::StreamedText, &accepted).is_success());
    assert!(matches!(classify(ProbeCase::StreamedText, &rejected), ProbeOutcome::DecodeFailure { .. }));
}

proptest! {
    #[test]
    fn arbitrary_unicode_prose_never_becomes_a_custom_call(prose in any::<String>()) {
        let tool_looking_prose = format!("fixture_raw custom_tool_call call_id OK {prose}");
        let response = completed_response(
            serde_json::json!([{"type":"message","content":[{"type":"output_text","text":tool_looking_prose}]}]),
        );

        let outcome = classify(ProbeCase::CustomNamed, &response);
        prop_assert!(
            matches!(&outcome, ProbeOutcome::NoCall { .. }),
            "tool-looking prose classified as an executable outcome: {:?}",
            outcome
        );
    }

    #[test]
    fn incomplete_terminal_status_never_succeeds(status_suffix in any::<String>()) {
        let response = response_with_status(
            &format!("incomplete-{status_suffix}"),
            serde_json::json!([{"type":"function_call","name":"fixture_echo","call_id":"final-call","arguments":"{\"text\":\"OK\"}"}]),
        );

        let outcome = classify(ProbeCase::Function, &response);
        prop_assert!(
            !outcome.is_success(),
            "incomplete terminal status classified as success: {:?}",
            outcome
        );
    }

    #[test]
    fn custom_input_and_final_id_are_exact_or_rejected(call_id in "[^\\p{C}]{1,64}", suffix in any::<String>()) {
        let accepted = completed_response(
            serde_json::json!([{"type":"custom_tool_call","name":"fixture_raw","call_id":call_id.clone(),"input":"OK"}]),
        );
        let accepted_outcome = classify(ProbeCase::CustomNamed, &accepted);
        prop_assert!(
            matches!(
            &accepted_outcome,
            ProbeOutcome::ToolCallSuccess { call_id: actual_call_id, input, .. }
                if actual_call_id == &call_id
                    && input.as_str().is_some_and(|raw| raw.as_bytes() == b"OK")
            ),
            "exact custom payload or final ID was not preserved: {:?}",
            accepted_outcome
        );

        let altered_input = format!("not-OK-{suffix}");
        let altered = completed_response(
            serde_json::json!([{"type":"custom_tool_call","name":"fixture_raw","call_id":call_id,"input":altered_input}]),
        );
        let altered_outcome = classify(ProbeCase::CustomNamed, &altered);
        prop_assert!(
            matches!(&altered_outcome, ProbeOutcome::DecodeFailure { .. }),
            "altered custom payload was not rejected: {:?}",
            altered_outcome
        );
    }
}

#[test]
fn captured_custom_failures_and_no_call_completions_stay_distinct() -> anyhow::Result<()> {
    for stem in ["custom", "custom-format-required"] {
        let response = captured_response(stem, 500, "sse")?;
        assert!(matches!(
            classify(ProbeCase::CustomRequired, &response),
            ProbeOutcome::HttpFailure { status: 500, .. }
        ));
    }
    for (stem, case) in [
        ("custom-named", ProbeCase::CustomNamed),
        ("custom-text-format", ProbeCase::CustomNamedTextFormat),
    ] {
        let response = captured_response(stem, 200, "sse")?;
        assert!(matches!(classify(case, &response), ProbeOutcome::NoCall { .. }));
        assert!(reported_usage(&response).is_some(), "{stem} should retain reported usage");
    }
    Ok(())
}

#[test]
fn all_sanitized_requests_are_synthetic_and_headers_exclude_credentials() {
    for stem in [
        "function",
        "custom",
        "custom-format-required",
        "custom-named",
        "custom-text-format",
        "planner",
    ] {
        let base = format!("{}/{FIXTURES}/{stem}", env!("CARGO_MANIFEST_DIR"));
        let request: Value = serde_json::from_slice(
            &std::fs::read(format!("{base}-request.json")).expect("read sanitized request fixture"),
        )
        .expect("sanitized request fixture is JSON");
        let serialized = request.to_string();
        assert!(serialized.contains("GLM-5.3-Flash"));
        assert!(!serialized.to_ascii_lowercase().contains("authorization"));
        let headers = std::fs::read_to_string(format!("{base}-headers.txt")).expect("read sanitized response headers");
        let normalized = headers.to_ascii_lowercase();
        assert!(!normalized.contains("authorization"));
        assert!(!normalized.contains("api-key"));
    }
}

#[test]
fn paid_case_selection_enforces_unique_bounded_explicit_cases() {
    assert_eq!(
        parse_selected_cases("buffered-text,function,custom-named").expect("valid paid case selection"),
        [ProbeCase::BufferedText, ProbeCase::Function, ProbeCase::CustomNamed]
    );
    for invalid in [
        "",
        "unknown",
        "function,function",
        "buffered-text,streamed-text,function,custom-required,custom-named",
    ] {
        assert!(parse_selected_cases(invalid).is_err(), "selection {invalid:?} must be rejected");
    }
}

#[test]
fn every_case_respects_the_per_request_cap_and_only_buffered_text_disables_streaming() {
    for case in [
        ProbeCase::BufferedText,
        ProbeCase::StreamedText,
        ProbeCase::Function,
        ProbeCase::CustomRequired,
        ProbeCase::CustomRequiredTextFormat,
        ProbeCase::CustomNamed,
        ProbeCase::CustomNamedTextFormat,
    ] {
        let request = case.request();
        assert_eq!(request["max_output_tokens"], OUTPUT_CAP);
        assert_eq!(request["stream"], case != ProbeCase::BufferedText);
    }
}

#[tokio::test]
async fn partial_response_is_captured_before_read_failure_is_reported() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind partial response fixture");
    let address = listener.local_addr().expect("partial response address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept probe request");
        let mut request = vec![0_u8; 8_192];
        let _bytes_read = socket.read(&mut request).await.expect("read probe request");
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 999\r\nX-Ratelimit-Limit-Requests: 100\r\nAuthorization: Bearer must-not-be-captured\r\nX-Unrelated: must-not-be-captured\r\n\r\ndata: partial",
            )
            .await
            .expect("write truncated probe response");
    });
    let capture = tempfile::tempdir().expect("partial capture directory");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("partial response client");
    let response = send_case_to(
        &client,
        "synthetic-key",
        ProbeCase::StreamedText,
        0,
        capture.path(),
        &format!("http://{address}/responses"),
    )
    .await
    .expect("capture truncated response");
    server.await.expect("partial response server");

    assert_eq!(response.body, b"data: partial");
    assert_eq!(response.safe_headers.get("content-type").map(String::as_str), Some("text/event-stream"));
    assert_eq!(response.safe_headers.get("content-length").map(String::as_str), Some("999"));
    assert_eq!(response.safe_headers.get("x-ratelimit-limit-requests").map(String::as_str), Some("100"));
    assert!(!response.safe_headers.contains_key("authorization"));
    assert!(!response.safe_headers.contains_key("x-unrelated"));
    assert!(matches!(classify(ProbeCase::StreamedText, &response), ProbeOutcome::TransportFailure { .. }));
    assert_eq!(
        std::fs::read(capture.path().join("streamed-text/response.body")).expect("read partial raw capture"),
        b"data: partial"
    );
}

#[test]
fn a_failed_first_case_does_not_hide_later_selected_outcomes() {
    let selected = [ProbeCase::CustomRequired, ProbeCase::Function];
    let outcomes = selected.map(|case| {
        if case == ProbeCase::CustomRequired {
            ProbeOutcome::HttpFailure { status: 500, body: "synthetic failure".into() }
        } else {
            ProbeOutcome::ToolCallSuccess {
                tool_type: "function_call".into(),
                name: "fixture_echo".into(),
                call_id: "final-call".into(),
                input: serde_json::json!({"text":"OK"}),
            }
        }
    });
    let failures = selected
        .into_iter()
        .zip(&outcomes)
        .filter_map(|(case, outcome)| compatibility_failure(case, outcome))
        .collect::<Vec<_>>();

    assert_eq!(outcomes.len(), selected.len());
    assert!(!outcomes[0].is_success());
    assert!(outcomes[1].is_success());
    assert_eq!(failures.len(), 1);
    assert!(failures[0].starts_with("custom-required:"));
}
