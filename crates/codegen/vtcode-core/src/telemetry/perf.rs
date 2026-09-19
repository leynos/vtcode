use hashbrown::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use once_cell::sync::OnceCell;
use tokio::runtime::Handle;
use tracing::debug;

use crate::config::TelemetryConfig;
use crate::telemetry::{TelemetryEvent, TelemetryPipeline};

#[derive(Debug)]
pub struct PerfRecorder {
    enabled: AtomicBool,
    pipeline: Arc<TelemetryPipeline>,
}

static PERF_RECORDER: OnceCell<PerfRecorder> = OnceCell::new();

pub fn initialize_perf_telemetry(config: &TelemetryConfig) {
    let recorder = PerfRecorder {
        enabled: AtomicBool::new(config.perf_events),
        pipeline: Arc::new(TelemetryPipeline::new(config.clone())),
    };
    let _ = PERF_RECORDER.set(recorder);
}

pub fn enabled() -> bool {
    PERF_RECORDER
        .get()
        .map(|recorder| recorder.enabled.load(Ordering::Relaxed))
        .unwrap_or(false)
}

pub fn record_duration(name: &'static str, duration: Duration, tags: HashMap<String, String>) {
    record_value(name, duration.as_secs_f64() * 1000.0, tags);
}

pub fn record_value(name: &'static str, value: f64, tags: HashMap<String, String>) {
    let Some(recorder) = PERF_RECORDER.get() else {
        return;
    };
    if !recorder.enabled.load(Ordering::Relaxed) {
        return;
    }

    let event = metric_event(name, value, tags);

    if let Ok(handle) = Handle::try_current() {
        let pipeline = Arc::clone(&recorder.pipeline);
        handle.spawn(async move {
            let _ = pipeline.record(event).await;
        });
    } else {
        debug!(name, "Skipping perf event (no runtime)");
    }
}

fn metric_event(name: &'static str, value: f64, tags: HashMap<String, String>) -> TelemetryEvent {
    let mut event = TelemetryEvent::new(name, value);
    event.tags = tags;
    event
}

#[cfg(feature = "telemetry-test-support")]
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct TestPerfRecorder {
    pipeline: Arc<TelemetryPipeline>,
}

#[cfg(feature = "telemetry-test-support")]
impl Default for TestPerfRecorder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "telemetry-test-support")]
impl TestPerfRecorder {
    pub fn new() -> Self {
        Self {
            pipeline: Arc::new(TelemetryPipeline::new(TelemetryConfig::default())),
        }
    }

    pub async fn record_value(
        &self,
        name: &'static str,
        value: f64,
        tags: HashMap<String, String>,
    ) -> anyhow::Result<()> {
        self.pipeline.record(metric_event(name, value, tags)).await
    }

    pub async fn drain(&self) -> Vec<TelemetryEvent> {
        self.pipeline.snapshot().await
    }
}

#[cfg(all(test, feature = "telemetry-test-support"))]
mod tests {
    use super::TestPerfRecorder;
    use hashbrown::HashMap;

    #[tokio::test]
    async fn test_perf_recorder_records_and_drains_awaited_event() {
        let recorder = TestPerfRecorder::new();
        let tags = HashMap::from([
            ("mode".to_owned(), "buffered".to_owned()),
            ("provider_class".to_owned(), "builtin".to_owned()),
        ]);

        recorder
            .record_value("vtcode.test.metric", 42.0, tags.clone())
            .await
            .expect("recording an event should succeed");

        let events = recorder.drain().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "vtcode.test.metric");
        assert_eq!(events[0].value.to_bits(), 42.0_f64.to_bits());
        assert_eq!(events[0].tags, tags);
        assert!(recorder.drain().await.is_empty());
    }
}

pub struct PerfSpan {
    name: &'static str,
    start: Instant,
    tags: HashMap<String, String>,
    enabled: bool,
}

impl PerfSpan {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
            tags: HashMap::new(),
            enabled: enabled(),
        }
    }

    pub fn tag(&mut self, key: impl Into<String>, value: impl Into<String>) {
        if self.enabled {
            self.tags.insert(key.into(), value.into());
        }
    }
}

impl Drop for PerfSpan {
    fn drop(&mut self) {
        if self.enabled {
            record_duration(self.name, self.start.elapsed(), std::mem::take(&mut self.tags));
        }
    }
}
