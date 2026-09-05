//! Opt-in, bounded Friendli Responses probes. Returned tools are never executed.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde_json::json;
use support::friendli_probe::{
    CapturedResponse, ProbeCase, ProbeOutcome, compatibility_failure, parse_selected_cases, reported_usage,
    send_case_to,
};
use vtcode_config::core::CustomProviderRequestPolicyConfig;

fn selected_cases() -> Result<Vec<ProbeCase>> {
    let raw = std::env::var("VTCODE_FRIENDLI_PROBE_CASES")
        .context("set VTCODE_FRIENDLI_PROBE_CASES; implicit paid cases are forbidden")?;
    parse_selected_cases(&raw)
}

fn private_capture_dir() -> Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os("VTCODE_FRIENDLI_CAPTURE_DIR")
            .context("set VTCODE_FRIENDLI_CAPTURE_DIR to an explicit private output directory")?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .with_context(|| format!("atomically create private capture directory {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&path)
            .with_context(|| format!("atomically create private capture directory {}", path.display()))?;
    }
    Ok(path)
}

fn write_capture(directory: &Path, case: ProbeCase, response: &CapturedResponse, outcome: &ProbeOutcome) -> Result<()> {
    let case_dir = directory.join(case.label());
    std::fs::write(
        case_dir.join("response-meta.json"),
        serde_json::to_vec_pretty(&json!({
            "status": response.status,
            "safe_headers": &response.safe_headers,
            "usage": reported_usage(response),
            "transport_error": &response.transport_error,
        }))?,
    )?;
    std::fs::write(case_dir.join("outcome.json"), serde_json::to_vec_pretty(outcome)?)?;
    Ok(())
}

#[tokio::test]
#[ignore = "paid Friendli API; explicit case/key/capture-directory opt-in required"]
async fn friendli_responses_selected_compatibility_probes() -> Result<()> {
    ensure!(std::env::var("VTCODE_FRIENDLI_LIVE").as_deref() == Ok("1"), "explicit live opt-in is required");
    let key_path = PathBuf::from(std::env::var_os("VTCODE_FRIENDLI_LIVE_KEY_FILE").context("set key-file path")?);
    let key = std::fs::read_to_string(key_path).context("read Friendli key file")?;
    ensure!(!key.trim().is_empty(), "Friendli key file is empty");
    let cases = selected_cases()?;
    let capture_dir = private_capture_dir()?;
    let request_policy = CustomProviderRequestPolicyConfig {
        max_retries: 0,
        ..CustomProviderRequestPolicyConfig::default()
    };
    ensure!(request_policy.max_retries == 0, "paid probe must not retry");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(180))
        .build()?;
    let mut failed_cases = Vec::new();

    for case in cases {
        let started = Instant::now();
        let response = send_case_to(
            &client,
            key.trim(),
            case,
            request_policy.max_retries,
            &capture_dir,
            "https://api.friendli.ai/serverless/v1/responses",
        )
        .await?;
        let outcome = support::friendli_probe::classify(case, &response);
        write_capture(&capture_dir, case, &response, &outcome)?;
        println!(
            "{}",
            json!({
                "probe": case.label(),
                "elapsed_ms": started.elapsed().as_millis(),
                "status": response.status,
                "usage": reported_usage(&response),
                "outcome": outcome,
            })
        );
        if let Some(failure) = compatibility_failure(case, &outcome) {
            failed_cases.push(failure);
        }
    }
    ensure!(failed_cases.is_empty(), "compatibility failures: {}", failed_cases.join("; "));
    Ok(())
}
