//! Log-capture fixtures shared by ACP privacy regressions.
//!
//! The helpers install a scoped subscriber for the duration of the tested
//! operation. They never change the global subscriber, so a test can assert on
//! exactly the events the operation emitted without leaking capture into other
//! tests.
use std::fmt::Write as FmtWrite;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

/// Collect every emitted log event so tests can assert on the exact fields
/// that reach the diagnostics pipeline.
#[derive(Clone, Default)]
pub(super) struct LogCapture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for LogCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        let mut fields = CapturedFields::default();
        event.record(&mut fields);
        self.0.lock().expect("log capture lock").push(fields.0);
    }
}

#[derive(Default)]
struct CapturedFields(String);

impl Visit for CapturedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        write!(&mut self.0, " {}={value}", field.name()).expect("write trace field");
    }
}

/// Run `run` with log capture installed and return its output plus the events
/// admitted at `level` and above.
pub(super) fn capturing_at<T>(level: LevelFilter, run: impl FnOnce() -> T) -> (T, Vec<String>) {
    let capture = LogCapture::default();
    let events = Arc::clone(&capture.0);
    let subscriber = Registry::default().with(level).with(capture);
    let output = tracing::subscriber::with_default(subscriber, run);
    let events = events.lock().expect("log capture lock").clone();
    (output, events)
}

/// Capture at the default operator-visible severity.
///
/// INFO is the level at which the ACP runtime's default diagnostics become
/// visible, so events admitted here are the ones an operator sees by default.
pub(super) fn capturing<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
    capturing_at(LevelFilter::INFO, run)
}

/// Poll `future` under a scoped subscriber that stays attached across awaits.
///
/// A synchronous `with_default` closure cannot span an await point, so the
/// subscriber is installed with `set_default` instead. That keeps capture live
/// while the future is polled and still limits it to the calling thread.
pub(super) async fn capturing_async<T>(level: LevelFilter, future: impl Future<Output = T>) -> (T, Vec<String>) {
    let capture = LogCapture::default();
    let events = Arc::clone(&capture.0);
    let subscriber = Registry::default().with(level).with(capture);
    let guard = tracing::subscriber::set_default(subscriber);
    let output = future.await;
    drop(guard);
    let events = events.lock().expect("log capture lock").clone();
    (output, events)
}
