//! Owns the isolated local VidaiMock process and its recording HTTP proxy.
//!
//! Requests remain local, are forwarded unchanged to the version-checked fixture,
//! and are captured for scenario assertions. Owners stop their own process/task
//! on drop; the explicit proxy finish path also joins its task.

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::routing::post;
use serde_json::Value;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(super) struct RunningVidaiMock(Child);

impl Drop for RunningVidaiMock {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

impl RunningVidaiMock {
    pub(super) async fn start() -> (Self, String) {
        let version = Command::new("vidaimock").arg("--version").output().expect("VidaiMock version");
        assert!(version.status.success());
        let version_text = String::from_utf8_lossy(&version.stdout);
        assert!(
            version_text.contains("0.3.1") || version_text.contains("0.4.0"),
            "unsupported VidaiMock: {version_text}"
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve VidaiMock port");
        let port = listener.local_addr().expect("VidaiMock address").port();
        drop(listener);
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock");
        let mut server = Self(
            Command::new("vidaimock")
                .env("VIDAIMOCK_ISOLATED", "true")
                .args(["--host", "127.0.0.1", "--port", &port.to_string()])
                .arg("--config")
                .arg(fixtures.join("scenarios/trickled-stream.toml"))
                .arg("--config-dir")
                .arg(fixtures)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start VidaiMock issue164 fixture"),
        );
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return (server, format!("http://127.0.0.1:{port}/v1/issue164/chat/completions"));
            }
            assert!(server.0.try_wait().expect("poll VidaiMock").is_none(), "VidaiMock exited at startup");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("VidaiMock did not become ready");
    }
}

#[derive(Clone)]
struct RecordingProxy {
    upstream: String,
    client: reqwest::Client,
    requests: Arc<Mutex<Vec<Value>>>,
    normal_requests: Arc<AtomicUsize>,
}

pub(super) fn is_summary(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|messages| {
        messages.len() == 1
            && messages[0]["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("Summary requested at "))
    })
}

async fn forward_request(
    State(proxy): State<RecordingProxy>,
    bytes: Bytes,
) -> (StatusCode, [(header::HeaderName, &'static str); 1], String) {
    let body: Value = serde_json::from_slice(&bytes).expect("provider request JSON");
    let summary = is_summary(&body);
    let step = if summary {
        0
    } else {
        proxy.normal_requests.fetch_add(1, Ordering::SeqCst)
    };
    proxy.requests.lock().expect("record HTTP requests").push(body);
    let response = proxy
        .client
        .post(&proxy.upstream)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-issue164-step", step.to_string())
        .header("x-issue164-summary", if summary { "true" } else { "false" })
        .body(bytes)
        .send()
        .await
        .expect("forward request to local VidaiMock");
    let status = response.status();
    let text = response.text().await.expect("read VidaiMock response");
    (status, [(header::CONTENT_TYPE, "application/json")], text)
}

pub(super) struct RecordingProxyServer {
    pub(super) url: String,
    task: tokio::task::JoinHandle<()>,
}

impl RecordingProxyServer {
    pub(super) async fn start(upstream: String, requests: Arc<Mutex<Vec<Value>>>) -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("recording proxy");
        let url = format!("http://{}/v1", listener.local_addr().expect("proxy address"));
        let proxy = RecordingProxy {
            upstream,
            client: reqwest::Client::new(),
            requests,
            normal_requests: Arc::new(AtomicUsize::new(0)),
        };
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(forward_request))
                    .with_state(proxy),
            )
            .await
            .expect("serve recording proxy");
        });
        Self { url, task }
    }

    pub(super) async fn finish(self) {
        self.task.abort();
        // Borrow the handle so Drop still owns panic-path cleanup.
        let mut task = self;
        drop((&mut task.task).await);
    }
}

impl Drop for RecordingProxyServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
