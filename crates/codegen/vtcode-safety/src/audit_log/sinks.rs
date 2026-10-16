//! Store audit entries in null, memory, file, or fan-out sinks.

use super::diagnostics::log_io_failure;
use super::{ToolAuditEntry, ToolAuditSink};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use vtcode_commons::VtCodePaths;

/// Null sink — accepts every entry but discards them.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl ToolAuditSink for NullSink {
    fn write(&self, _entry: &ToolAuditEntry) -> std::io::Result<()> {
        Ok(())
    }
}

/// In-memory sink — useful for tests and ephemeral tooling.
#[derive(Debug, Default, Clone)]
pub struct InMemorySink {
    entries: Arc<Mutex<Vec<ToolAuditEntry>>>,
}

impl InMemorySink {
    /// Create a new in-memory sink with no entries.
    #[must_use]
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn lock_entries(&self) -> MutexGuard<'_, Vec<ToolAuditEntry>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Snapshot all entries recorded so far.
    pub(super) fn entries(&self) -> Vec<ToolAuditEntry> {
        self.lock_entries().clone()
    }

    /// Number of recorded entries.
    pub(super) fn len(&self) -> usize {
        self.lock_entries().len()
    }

    /// Returns true when no entries have been recorded.
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop all recorded entries.
    pub(super) fn clear(&self) {
        self.lock_entries().clear();
    }
}

impl ToolAuditSink for InMemorySink {
    fn write(&self, entry: &ToolAuditEntry) -> std::io::Result<()> {
        self.lock_entries().push(entry.clone());
        Ok(())
    }
}

/// Append-only JSONL file sink with size-based rotation.
///
/// The sink writes each entry as a single JSON line. When the current file
/// exceeds `max_size_bytes`, it is renamed to `<path>.1` (and previous
/// generations shift up to `<path>.N`) — keeping at most `max_files` files
/// on disk.
pub struct JsonlFileSink {
    pub(super) path: PathBuf,
    pub(super) max_size_bytes: u64,
    pub(super) max_files: usize,
    pub(super) state: Mutex<JsonlFileState>,
}

pub(super) struct JsonlFileState {
    pub(super) writer: Option<BufWriter<File>>,
    pub(super) bytes_written: u64,
}

impl std::fmt::Debug for JsonlFileSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlFileSink")
            .field("path", &self.path)
            .field("max_size_bytes", &self.max_size_bytes)
            .field("max_files", &self.max_files)
            .finish_non_exhaustive()
    }
}

impl JsonlFileSink {
    /// Open (or create) the sink at `path`. Returns an error if the file can't
    /// be created — typically a permissions issue.
    pub(super) fn open(path: impl Into<PathBuf>, max_size_bytes: u64, max_files: usize) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            VtCodePaths::ensure_user_dir(parent)
                .map_err(std::io::Error::other)
                .map(|_| ())?;
        }
        let file = VtCodePaths::open_private_append_file(&path).map_err(std::io::Error::other)?;
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            max_size_bytes,
            max_files: max_files.max(1),
            state: Mutex::new(JsonlFileState { writer: Some(BufWriter::new(file)), bytes_written }),
        })
    }

    /// Path of the active JSONL file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn rotate_if_needed(
        state: &mut JsonlFileState,
        path: &Path,
        max_size_bytes: u64,
        max_files: usize,
    ) -> std::io::Result<()> {
        if state.bytes_written < max_size_bytes {
            return Ok(());
        }
        // Persist buffered entries before dropping the writer for rotation.
        if let Some(writer) = state.writer.as_mut() {
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }
        drop(state.writer.take());
        // Shift generations: <path>.N-1 -> <path>.N, …, <path> -> <path>.1
        for index in (1..max_files).rev() {
            let from = rotated_path(path, index);
            let to = rotated_path(path, index + 1);
            if from.exists() {
                std::fs::rename(&from, &to)?;
            }
        }
        if path.exists() {
            std::fs::rename(path, rotated_path(path, 1))?;
        }
        let file = VtCodePaths::open_private_append_file(path).map_err(std::io::Error::other)?;
        state.writer = Some(BufWriter::new(file));
        state.bytes_written = 0;
        Ok(())
    }
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{index}"));
    PathBuf::from(s)
}

impl ToolAuditSink for JsonlFileSink {
    fn write(&self, entry: &ToolAuditEntry) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let serialized = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        let line_length = serialized.len() as u64 + 1;
        Self::rotate_if_needed(&mut state, &self.path, self.max_size_bytes, self.max_files)
            .inspect_err(|error| log_io_failure("rotation", error))?;
        let writer = state
            .writer
            .as_mut()
            .ok_or_else(|| std::io::Error::other("audit writer unavailable"))?;
        writeln!(writer, "{serialized}").inspect_err(|error| log_io_failure("write", error))?;
        state.bytes_written = state.bytes_written.saturating_add(line_length);
        Ok(())
    }

    fn flush(&self) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let writer = state
            .writer
            .as_mut()
            .ok_or_else(|| std::io::Error::other("audit writer unavailable"))?;
        writer.flush().inspect_err(|error| log_io_failure("flush", error))?;
        writer
            .get_ref()
            .sync_all()
            .inspect_err(|error| log_io_failure("durable_sync", error))
    }
}

impl Drop for JsonlFileSink {
    fn drop(&mut self) {
        // Drop cannot propagate errors; explicit flush is the durability boundary.
        if let Err(error) = self.flush() {
            log_io_failure("drop_flush", &error);
        }
    }
}

/// Fan-out sink — forwards every entry to a list of inner sinks.
///
/// Sinks are called sequentially in registration order. All sinks are attempted;
/// the first I/O failure is returned to the caller.
pub struct MultiSink {
    inner: Vec<Arc<dyn ToolAuditSink>>,
}

impl std::fmt::Debug for MultiSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiSink").field("sink_count", &self.inner.len()).finish()
    }
}

impl MultiSink {
    /// Create a fan-out sink. The slice is moved into `Arc`s to allow sharing
    /// with callers that want to read from one of the sinks while still
    /// forwarding writes.
    #[must_use]
    pub(super) fn new(sinks: Vec<Arc<dyn ToolAuditSink>>) -> Self {
        Self { inner: sinks }
    }

    /// Number of inner sinks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true when no inner sinks are registered.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl ToolAuditSink for MultiSink {
    fn write(&self, entry: &ToolAuditEntry) -> std::io::Result<()> {
        let mut first_error = None;
        for sink in &self.inner {
            if let Err(error) = sink.write(entry) {
                let _first_error = first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn flush(&self) -> std::io::Result<()> {
        let mut first_error = None;
        for sink in &self.inner {
            if let Err(error) = sink.flush() {
                let _first_error = first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::InMemorySink;
    use std::sync::Arc;

    #[test]
    fn in_memory_sink_recovers_a_poisoned_lock() {
        let sink = InMemorySink::new();
        let entries = Arc::clone(&sink.entries);
        let poison_result = std::thread::spawn(move || {
            let _entries = entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            panic!("poison the in-memory audit sink lock");
        })
        .join();

        assert!(poison_result.is_err());
        assert!(sink.entries().is_empty());
        assert_eq!(sink.len(), 0);
        sink.clear();
        assert!(sink.is_empty());
    }
}
