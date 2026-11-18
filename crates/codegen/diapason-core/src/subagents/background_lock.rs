//! The workspace background ledger's inter-process lock.
//!
//! The lock is advisory and held on a sidecar file next to the ledger, so it
//! serialises a reload, merge and write across every Diapason process that
//! shares the workspace, which no in-process mutex can do.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

/// The path of the sidecar file the ledger lock is taken on.
fn background_state_lock_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(diapason_commons::workspace_dir_name())
        .join("state")
        .join("background_subagents.lock")
}

/// Holds the workspace ledger's inter-process lock until it is dropped.
///
/// The lock is advisory and process-wide: it serialises the reload, merge and
/// write of the shared ledger across every Diapason process that shares the
/// workspace, which the in-process mutex cannot do.
pub(crate) struct BackgroundStateLock {
    file: std::fs::File,
}

impl Drop for BackgroundStateLock {
    fn drop(&mut self) {
        // `flock(LOCK_UN)` never blocks, so releasing here cannot stall the
        // async runtime. A failure leaves the lock to be released when the
        // descriptor closes, which is the same outcome.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// How long one save waits for another process to release the ledger lock.
///
/// The wait is bounded because the holder may be a suspended or stalled
/// process: a blocking acquisition would then park the save, and the shared
/// in-process persistence mutex it holds, for as long as that process lives.
const BACKGROUND_STATE_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// The delay before the first lock retry; each further retry doubles it.
const BACKGROUND_STATE_LOCK_INITIAL_BACKOFF: Duration = Duration::from_millis(5);

/// The largest delay between two lock retries.
const BACKGROUND_STATE_LOCK_MAX_BACKOFF: Duration = Duration::from_millis(250);

/// Acquires the workspace ledger's inter-process exclusive lock.
///
/// Two processes that each read the ledger before either writes can lose a
/// record: the later atomic rename replaces the file with a snapshot that
/// never saw the other's record. Callers must hold this guard across the
/// reload, merge and write of one save.
///
/// Contention is retried without blocking the runtime until
/// [`BACKGROUND_STATE_LOCK_TIMEOUT`] expires, so a lock held by a stalled
/// process fails the save with a contextual error instead of parking it. A
/// caller that never acquires the lock must not write the ledger.
pub(crate) async fn lock_background_state(workspace_root: &Path) -> Result<BackgroundStateLock> {
    lock_background_state_within(workspace_root, BACKGROUND_STATE_LOCK_TIMEOUT).await
}

/// Acquires the ledger lock, giving up after `timeout`.
///
/// Production saves use [`BACKGROUND_STATE_LOCK_TIMEOUT`]. A test supplies a
/// short deadline so it can observe the expiry without waiting in real time.
pub(crate) async fn lock_background_state_within(
    workspace_root: &Path,
    timeout: Duration,
) -> Result<BackgroundStateLock> {
    let path = background_state_lock_path(workspace_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let opened_path = path.clone();
    let file = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&opened_path)
            .with_context(|| format!("Failed to open {}", opened_path.display()))
    })
    .await
    .context("Failed to join the background state lock task")??;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut backoff = BACKGROUND_STATE_LOCK_INITIAL_BACKOFF;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(BackgroundStateLock { file }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(error).with_context(|| {
                        format!(
                            "Timed out after {}ms waiting for the background state lock {}",
                            timeout.as_millis(),
                            path.display()
                        )
                    });
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKGROUND_STATE_LOCK_MAX_BACKOFF);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to lock {}", path.display()));
            }
        }
    }
}
