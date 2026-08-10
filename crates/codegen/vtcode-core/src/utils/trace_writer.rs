//! Shared buffered trace log writer with flush-on-exit support.
//!
//! Provides a `BufWriter`-backed file writer wrapped in `Arc<Mutex<..>>` so the
//! tracing `fmt::layer` can write efficiently (batched syscalls) while still
//! allowing an explicit `flush_trace_log()` call on process exit or signal.

use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};

/// Capacity of the internal `LineWriter`.
const BUF_CAPACITY: usize = 64 * 1024;

/// Global handle to the active trace log writer so `flush_trace_log` can be
/// called from signal handlers / shutdown hooks without threading the writer
/// through the entire call stack.
static GLOBAL_WRITER: OnceLock<FlushableWriter> = OnceLock::new();

/// A clonable, thread-safe buffered writer that implements `std::io::Write`
/// so it can be passed directly to `tracing_subscriber::fmt::layer().with_writer(..)`.
#[derive(Clone)]
pub struct FlushableWriter {
    inner: Arc<Mutex<LineWriter<File>>>,
}

impl FlushableWriter {
    /// Open (or create) a log file and wrap it in a buffered writer.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open trace log file: {}", path.display()))?;
        let writer = LineWriter::with_capacity(BUF_CAPACITY, file);
        let flushable = Self { inner: Arc::new(Mutex::new(writer)) };
        // Store globally so `flush_trace_log` works from anywhere.
        let _ = GLOBAL_WRITER.set(flushable.clone());
        // Register the flush hook in vtcode-commons so crates that don't
        // depend on vtcode-core (e.g. vtcode-ui) can still trigger a flush.
        vtcode_commons::trace_flush::register_trace_flush_hook(flush_trace_log);
        Ok(flushable)
    }

    /// Flush the internal buffer to disk.
    pub fn flush(&self) {
        let _ = self.flush_locked();
    }

    fn lock_writer(&self) -> std::io::Result<MutexGuard<'_, LineWriter<File>>> {
        self.inner
            .lock()
            .map_err(|e| std::io::Error::other(format!("trace writer lock poisoned: {e}")))
    }

    fn flush_locked(&self) -> std::io::Result<()> {
        let mut guard = self.lock_writer()?;
        guard.flush()
    }
}

impl Write for FlushableWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self.lock_writer()?;
        guard.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_locked()
    }
}

/// Flush the global trace log writer to disk.
///
/// Safe to call from signal handlers, shutdown hooks, or `Drop` implementations.
/// No-op if no trace writer has been initialized.
pub fn flush_trace_log() {
    if let Some(writer) = GLOBAL_WRITER.get() {
        writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::FlushableWriter;
    use std::io::Write;

    #[test]
    fn newline_terminated_events_are_visible_without_explicit_flush() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let log_path = temp_dir.path().join("debug.log");
        let mut writer = FlushableWriter::open(&log_path)?;

        writer.write_all(b"ACP request started\n")?;

        assert_eq!(std::fs::read_to_string(log_path)?, "ACP request started\n");
        Ok(())
    }
}
