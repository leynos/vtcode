use anyhow::{Context, Result};
#[cfg(test)]
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::{BufWriter, Write};
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::sync::{Arc, Mutex};

use diapason_core::utils::async_line_writer::{AsyncLineWriter, AsyncLineWriterDiagnostics};
#[cfg(test)]
use diapason_core::utils::file_utils::ensure_dir_exists_sync;
#[cfg(test)]
use tokio::task::spawn_blocking;

/// Optional JSONL compatibility/export writer.
pub(crate) enum LegacyWriter {
    Async(AsyncLineWriter),
    #[cfg(test)]
    Sync(Arc<Mutex<BufWriter<File>>>),
}

impl LegacyWriter {
    /// Open the non-blocking production writer.
    pub(crate) async fn new_async(path: std::path::PathBuf) -> Result<Self> {
        Ok(Self::Async(AsyncLineWriter::new_async(path).await?))
    }

    #[cfg(test)]
    pub(crate) fn new_sync(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            ensure_dir_exists_sync(parent)
                .with_context(|| format!("Failed to create harness log dir {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open harness log {}", path.display()))?;
        Ok(Self::Sync(Arc::new(Mutex::new(BufWriter::new(file)))))
    }

    /// Queue a line without performing production filesystem I/O on the caller.
    pub(crate) fn write_line(&self, line: String) -> Result<()> {
        match self {
            Self::Async(writer) => {
                writer.write_line(line);
                Ok(())
            }
            #[cfg(test)]
            Self::Sync(writer) => {
                let mut writer = writer
                    .lock()
                    .map_err(|error| anyhow::anyhow!("legacy harness writer lock poisoned: {error}"))?;
                writeln!(writer, "{line}").context("failed to write legacy harness event")?;
                writer.flush().context("failed to flush legacy harness test writer")?;
                Ok(())
            }
        }
    }

    /// Flush queued lines and report actor/file failures.
    pub(crate) async fn flush(&self) -> Result<()> {
        match self {
            Self::Async(writer) => writer.flush_result().await.context("failed to flush legacy harness exporter"),
            #[cfg(test)]
            Self::Sync(writer) => {
                let writer = Arc::clone(writer);
                spawn_blocking(move || {
                    let mut writer = writer.lock().map_err(|error| {
                        std::io::Error::other(format!("legacy harness writer lock poisoned: {error}"))
                    })?;
                    writer.flush()
                })
                .await
                .context("legacy harness flush task failed")??;
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn flush_sync(&self) -> Result<()> {
        match self {
            Self::Async(_) => Ok(()),
            Self::Sync(writer) => {
                let mut writer = writer
                    .lock()
                    .map_err(|error| anyhow::anyhow!("legacy harness writer lock poisoned: {error}"))?;
                writer.flush().context("failed to flush legacy harness exporter")
            }
        }
    }

    pub(crate) fn diagnostics(&self) -> AsyncLineWriterDiagnostics {
        match self {
            Self::Async(writer) => writer.diagnostics(),
            #[cfg(test)]
            Self::Sync(_) => AsyncLineWriterDiagnostics::default(),
        }
    }
}
