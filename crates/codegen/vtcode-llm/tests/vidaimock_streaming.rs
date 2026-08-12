//! VidaiMock-backed streaming physics tests for the OpenAI-chat adapter.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use vtcode_config::core::{AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig};
use vtcode_llm::provider::{LLMProvider, LLMRequest, LLMStreamEvent, Message};
use vtcode_llm::providers::CustomProviderBackendRouter;

const MODEL: &str = "DeepSeek-V4-Flash-0731";

struct RunningVidaiMock {
    child: Child,
    base_url: String,
}

impl RunningVidaiMock {
    fn start(scenario: &str) -> Result<Self> {
        let version = Command::new("vidaimock")
            .arg("--version")
            .output()
            .context("VidaiMock 0.1.3 is required for the ignored physics tests")?;
        assert!(version.status.success(), "vidaimock --version should succeed");
        assert!(
            String::from_utf8_lossy(&version.stdout).contains("0.1.3"),
            "physics fixtures are pinned to VidaiMock 0.1.3"
        );

        let listener = TcpListener::bind(("127.0.0.1", 0)).context("reserve VidaiMock port")?;
        let port = listener.local_addr().context("read reserved address")?.port();
        drop(listener);

        let fixtures = fixture_root();
        let mut child = Command::new("vidaimock")
            .env("VIDAIMOCK_ISOLATED", "true")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(fixtures.join("scenarios").join(scenario))
            .arg("--config-dir")
            .arg(&fixtures)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start VidaiMock")?;

        let address = ("127.0.0.1", port);
        for _ in 0..100 {
            if TcpStream::connect(address).is_ok() {
                return Ok(Self {
                    child,
                    base_url: format!("http://127.0.0.1:{port}/v1/physics"),
                });
            }
            if let Some(status) = child.try_wait().context("inspect VidaiMock process")? {
                bail!("VidaiMock exited before accepting requests: {status}");
            }
            thread::sleep(Duration::from_millis(25));
        }
        bail!("VidaiMock did not accept connections within 2.5 seconds");
    }
}

impl Drop for RunningVidaiMock {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

#[derive(Debug)]
struct StreamObservation {
    first_output_after: Option<Duration>,
    output_times: Vec<Duration>,
    content: String,
    completed: bool,
    error: Option<String>,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock")
}

fn provider(base_url: &str) -> CustomProviderBackendRouter {
    let config = CustomProviderConfig {
        name: "vidaimock-arli".to_owned(),
        display_name: "VidaiMock Arli".to_owned(),
        base_url: base_url.to_owned(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        model: MODEL.to_owned(),
        models: vec![MODEL.to_owned()],
        ..Default::default()
    };
    CustomProviderBackendRouter::from_config(
        config,
        Some("fixture-key".to_owned()),
        Some(MODEL.to_owned()),
        base_url.to_owned(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    )
}

async fn observe_stream(scenario: &str) -> Result<StreamObservation> {
    let server = RunningVidaiMock::start(scenario)?;
    let started_at = Instant::now();
    let stream_result = provider(&server.base_url)
        .stream(LLMRequest {
            model: MODEL.to_owned(),
            messages: vec![Message::user("Emit the fixture response".to_owned())].into(),
            stream: true,
            ..Default::default()
        })
        .await;
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            return Ok(StreamObservation {
                first_output_after: None,
                output_times: Vec::new(),
                content: String::new(),
                completed: false,
                error: Some(error.to_string()),
            });
        }
    };
    let mut observation = StreamObservation {
        first_output_after: None,
        output_times: Vec::new(),
        content: String::new(),
        completed: false,
        error: None,
    };

    while let Some(event) = stream.next().await {
        match event {
            Ok(LLMStreamEvent::Token { delta }) | Ok(LLMStreamEvent::Reasoning { delta }) if !delta.is_empty() => {
                let elapsed = started_at.elapsed();
                if observation.first_output_after.is_none() {
                    observation.first_output_after = Some(elapsed);
                }
                observation.output_times.push(elapsed);
                observation.content.push_str(&delta);
            }
            Ok(LLMStreamEvent::Completed { .. }) => observation.completed = true,
            Ok(_) => {}
            Err(error) => {
                observation.error = Some(error.to_string());
                break;
            }
        }
    }
    Ok(observation)
}

#[tokio::test]
#[ignore = "requires the pinned VidaiMock executable and real wall-clock streaming physics"]
async fn vidaimock_baseline_stream_completes_through_the_adapter() -> Result<()> {
    let observation = observe_stream("success.toml").await?;

    assert!(observation.error.is_none(), "unexpected stream failure: {observation:?}");
    assert!(observation.completed, "stream should complete: {observation:?}");
    assert_eq!(observation.content, "one two three four ");
    Ok(())
}

#[tokio::test]
#[ignore = "requires the pinned VidaiMock executable and real wall-clock streaming physics"]
async fn vidaimock_delays_the_first_adapter_output() -> Result<()> {
    let observation = observe_stream("delayed-first-token.toml").await?;

    assert!(observation.error.is_none(), "unexpected stream failure: {observation:?}");
    assert!(observation.completed, "stream should complete: {observation:?}");
    assert_eq!(observation.content, "one two three four ");
    assert!(
        observation
            .first_output_after
            .is_some_and(|elapsed| elapsed >= Duration::from_millis(4_500)),
        "the first adapter output should include configured TTFT: {observation:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires the pinned VidaiMock executable and real wall-clock streaming physics"]
async fn vidaimock_preserves_inter_token_cadence_through_the_adapter() -> Result<()> {
    let observation = observe_stream("trickled-stream.toml").await?;

    assert!(observation.error.is_none(), "unexpected stream failure: {observation:?}");
    assert!(observation.completed, "stream should complete: {observation:?}");
    assert_eq!(observation.output_times.len(), 4, "fixture should emit four content chunks");
    for times in observation.output_times.windows(2) {
        assert!(
            times[1].saturating_sub(times[0]) >= Duration::from_millis(200),
            "adapter collapsed VidaiMock's 250 ms stream cadence: {observation:?}"
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the pinned VidaiMock executable and real wall-clock streaming physics"]
async fn vidaimock_mid_stream_disconnect_surfaces_as_an_adapter_error() -> Result<()> {
    let observation = observe_stream("mid-stream-disconnect.toml").await?;

    assert!(!observation.completed, "disconnected stream must not complete: {observation:?}");
    assert!(observation.error.is_some(), "disconnect should surface as an error: {observation:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "requires the pinned VidaiMock executable and real wall-clock streaming physics"]
async fn vidaimock_provider_drop_surfaces_before_completion() -> Result<()> {
    let observation = observe_stream("provider-drop-500.toml").await?;

    assert!(!observation.completed, "dropped request must not complete: {observation:?}");
    assert!(observation.error.is_some(), "provider drop should surface as an error: {observation:?}");
    Ok(())
}
