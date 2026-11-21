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
pub(super) fn background_state_lock_path(workspace_root: &Path) -> PathBuf {
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

/// Returns whether `error` reports that another holder owns the ledger lock.
///
/// `flock(2)` signals contention with a platform-specific code, which `std`
/// normally maps to [`std::io::ErrorKind::WouldBlock`] and `fs2` also exposes
/// as [`fs2::lock_contended_error`]. Both forms are recognised so a contended
/// acquisition is retried on either mapping instead of failing the save on the
/// first attempt, which would drop a concurrent write. The raw codes are
/// compared only when both are present, so an error without an OS code is not
/// mistaken for contention.
fn is_lock_contention(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error
            .raw_os_error()
            .zip(fs2::lock_contended_error().raw_os_error())
            .is_some_and(|(actual, contended)| actual == contended)
}

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

/// Builds the error a waiter reports once its deadline has passed.
///
/// The message names the wait and the sidecar so a caller can tell a deadline
/// from an ordinary lock failure it should not retry.
fn lock_timeout_error(path: &Path, timeout: Duration) -> anyhow::Error {
    anyhow::anyhow!(
        "Timed out after {}ms waiting for the background state lock {}",
        timeout.as_millis(),
        path.display()
    )
}

/// Acquires the ledger lock, giving up after `timeout`.
///
/// Production saves use [`BACKGROUND_STATE_LOCK_TIMEOUT`]. A test supplies a
/// short deadline so it can observe the expiry without waiting in real time.
/// Every retry is made before the deadline: the backoff is capped at the
/// remaining budget and the deadline is re-checked after sleeping, so a timed
/// out wait reports the timeout instead of acquiring the lock late.
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
            Err(error) if is_lock_contention(&error) => {
                #[cfg(test)]
                notify_lock_contention(&path);
                // The wait is capped at the remaining budget, and the deadline
                // is re-checked before the next attempt. Checking only before
                // an uncapped sleep let a backoff outlive the deadline and then
                // acquire the lock on the retry after it.
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(lock_timeout_error(&path, timeout));
                }
                tokio::time::sleep(backoff.min(remaining)).await;
                if tokio::time::Instant::now() >= deadline {
                    return Err(lock_timeout_error(&path, timeout));
                }
                backoff = (backoff * 2).min(BACKGROUND_STATE_LOCK_MAX_BACKOFF);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to lock {}", path.display()));
            }
        }
    }
}

/// One test witness watching a workspace's ledger lock.
#[cfg(test)]
type ContentionWitness = (PathBuf, std::sync::Weak<tokio::sync::Notify>);

/// The witnesses notified when a waiter observes the ledger lock held.
///
/// The contention branch is otherwise unobservable from a test: a fixed sleep
/// before asserting can race the waiter's own start-up, so the assertion would
/// pass without the waiter ever reaching the branch. A witness is notified from
/// that branch, so a test releases its holder only after a real contention
/// attempt.
#[cfg(test)]
fn lock_contention_witnesses() -> &'static std::sync::Mutex<Vec<ContentionWitness>> {
    static WITNESSES: std::sync::OnceLock<std::sync::Mutex<Vec<ContentionWitness>>> = std::sync::OnceLock::new();
    WITNESSES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Registers a witness for the next contention on `workspace_root`'s lock.
#[cfg(test)]
fn observe_lock_contention(workspace_root: &Path) -> std::sync::Arc<tokio::sync::Notify> {
    let witness = std::sync::Arc::new(tokio::sync::Notify::new());
    let lock_path = background_state_lock_path(workspace_root);
    let mut witnesses = lock_contention_witnesses()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    witnesses.retain(|(_, registered)| registered.strong_count() > 0);
    witnesses.push((lock_path, std::sync::Arc::downgrade(&witness)));
    witness
}

/// Notifies every test witness watching `lock_path`.
#[cfg(test)]
fn notify_lock_contention(lock_path: &Path) {
    let witnesses = lock_contention_witnesses()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (watched_path, witness) in witnesses.iter() {
        if watched_path == lock_path
            && let Some(witness) = witness.upgrade()
        {
            witness.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every form `fs2` and `std` report contention in must be retried.
    ///
    /// A missed form turns a concurrent save into an immediate failure, and
    /// the ledger loses the record that save carried. The non-contention
    /// checks keep a real open or permission error from being retried until
    /// the deadline hides it.
    #[test]
    fn lock_contention_matches_every_platform_form() {
        assert!(is_lock_contention(&fs2::lock_contended_error()), "fs2's contention error must be retried");
        assert!(
            is_lock_contention(&std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            "the mapped contention kind must be retried"
        );
        assert!(
            !is_lock_contention(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            "an unrelated error must not be retried"
        );
    }

    /// A contended acquisition waits for the holder rather than failing.
    ///
    /// The sidecar lock is held by another process in production, so the
    /// save's `try_lock` reports contention. It must retry until the holder
    /// releases and then acquire, or a concurrent save would fail instead of
    /// serialising behind the other writer. The witness proves the waiter
    /// really reached the contention branch before the holder is released; a
    /// fixed sleep raced the waiter's start-up and asserted nothing.
    #[tokio::test]
    async fn lock_retries_after_the_holder_releases() {
        let workspace = tempfile::tempdir().expect("workspace");
        let held = lock_background_state_within(workspace.path(), Duration::from_secs(1))
            .await
            .expect("first lock");
        let witness = observe_lock_contention(workspace.path());
        let waiting_path = workspace.path().to_path_buf();
        let waiter =
            tokio::spawn(async move { lock_background_state_within(&waiting_path, Duration::from_secs(5)).await });
        tokio::time::timeout(Duration::from_secs(5), witness.notified())
            .await
            .expect("the waiter must observe the held lock before the holder is released");
        assert!(!waiter.is_finished(), "a waiter that observed contention must not have acquired the lock");

        drop(held);
        let acquired = waiter.await.expect("waiter task").expect("second lock");
        drop(acquired);
    }

    /// A retry must not acquire the lock after its deadline has passed.
    ///
    /// Backoff doubles past the remaining deadline, so a sleep that is not
    /// capped can wake after the holder released and still succeed. The
    /// caller asked for a bounded wait and cannot tell that guard arrived
    /// late, so the retry must be refused and the wait reported as a timeout.
    #[tokio::test]
    async fn lock_does_not_acquire_after_its_deadline() {
        let workspace = tempfile::tempdir().expect("workspace");
        let held = lock_background_state_within(workspace.path(), Duration::from_secs(1))
            .await
            .expect("first lock");
        // The holder outlives the waiter's deadline but releases before the
        // uncapped backoff would wake, which is exactly the late-acquire race.
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(70)).await;
            drop(held);
        });

        let started = tokio::time::Instant::now();
        let error = match lock_background_state_within(workspace.path(), Duration::from_millis(60)).await {
            Ok(_) => panic!("a retry must not acquire the lock after the deadline"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("Timed out"), "a late retry must report the deadline: {error:#}");
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "the wait must last for its deadline instead of failing early: {:?}",
            started.elapsed()
        );

        releaser.await.expect("the holder releases");
    }

    /// A lock that never becomes available fails with the deadline error.
    ///
    /// The wait is bounded so a stalled holder cannot park the save, and the
    /// shared in-process persistence mutex it holds, indefinitely. The error
    /// must name the timeout so the caller can tell a deadline from a lock
    /// failure it should not retry.
    #[tokio::test]
    async fn lock_times_out_while_the_holder_keeps_it() {
        let workspace = tempfile::tempdir().expect("workspace");
        let _held = lock_background_state_within(workspace.path(), Duration::from_secs(1))
            .await
            .expect("first lock");
        let started = tokio::time::Instant::now();
        let error = match lock_background_state_within(workspace.path(), Duration::from_millis(50)).await {
            Ok(_) => panic!("a held lock must not be acquired before its deadline"),
            Err(error) => error,
        };

        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(
            format!("{error:#}").contains("Timed out"),
            "the deadline failure must be reported as a timeout: {error:#}"
        );
    }
}
