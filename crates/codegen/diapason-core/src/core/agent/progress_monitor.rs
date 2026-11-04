//! Monotonic progress monitoring for long-horizon sessions.
//!
//! Agent capability is only useful if the agent *keeps getting closer to
//! completion over time*. [`ProgressMonitor`] externalizes that invariant into
//! a durable [`ProgressLedger`] persisted via `diapason-memory`, so the
//! harness can:
//!
//! - refuse to declare a session complete while tracked milestones are open,
//! - detect stalls (turns with no forward progress) and trigger
//!   compaction → replan → escalation, and
//! - resume a long task with an accurate picture of what is done.
//!
//! Persistence is best-effort: a failed disk write is logged, not fatal, so the
//! live run is never blocked by the progress side-channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use diapason_memory::progress::{Milestone, MilestoneStatus, ProgressLedger, load_progress, save_progress};
use tracing::warn;

/// Guard-rail interface isolating the [`ProgressMonitor`] from persistence IO.
///
/// The monitor owns *only* the progress domain logic; every side effect
/// (ledger persistence, memory checkpointing) is delegated through this trait.
/// This keeps the monitor unit-testable with an in-memory sink and prevents the
/// long-horizon progress logic from coupling to the filesystem or to
/// `diapason-memory` internals.
pub trait ProgressLedgerSink: Send + Sync {
    /// Persist the authoritative ledger. Best-effort; errors are the sink's
    /// concern (typically logged, not propagated).
    fn persist(&self, ledger: &ProgressLedger);

    /// Flush a human-readable checkpoint (e.g. `memories/progress.md`).
    /// Default: no-op, so sinks that do not support checkpoints opt out cleanly.
    fn checkpoint(&self, _ledger: &ProgressLedger) {}
}

/// A sink that discards everything — for tests and in-memory sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProgressSink;

impl ProgressLedgerSink for NullProgressSink {
    fn persist(&self, _ledger: &ProgressLedger) {}
}

/// Filesystem-backed sink: persists the ledger via `diapason-memory` and
/// checkpoints a markdown summary into the workspace's durable memory.
#[derive(Debug, Clone)]
pub struct SessionProgressSink {
    writer: PendingProgressWriter,
}

#[derive(Debug, Clone)]
struct PendingProgressWriter {
    signal: SyncSender<()>,
    pending: Arc<Mutex<PendingProgress>>,
}

#[derive(Debug, Default)]
struct PendingProgress {
    ledger: Option<ProgressLedger>,
    checkpoint: bool,
}

impl SessionProgressSink {
    /// Create a sink rooted at `workspace`.
    #[must_use]
    pub fn new(workspace: PathBuf) -> Self {
        let (signal, receiver) = mpsc::sync_channel(1);
        let pending = Arc::new(Mutex::new(PendingProgress::default()));
        let writer_workspace = workspace.clone();
        let writer_pending = Arc::clone(&pending);
        if let Err(error) = std::thread::Builder::new()
            .name("diapason-progress-writer".to_string())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    let work = match writer_pending.lock() {
                        Ok(mut pending) => PendingProgress {
                            ledger: pending.ledger.take(),
                            checkpoint: std::mem::take(&mut pending.checkpoint),
                        },
                        Err(error) => {
                            warn!(error = %error, "progress writer state lock poisoned");
                            continue;
                        }
                    };
                    persist_pending_progress(&writer_workspace, work);
                }
            })
        {
            warn!(error = %error, "failed to start progress writer thread");
        }

        Self { writer: PendingProgressWriter { signal, pending } }
    }
}

impl ProgressLedgerSink for SessionProgressSink {
    fn persist(&self, ledger: &ProgressLedger) {
        self.writer.enqueue(ledger, false);
    }

    fn checkpoint(&self, ledger: &ProgressLedger) {
        self.writer.enqueue(ledger, true);
    }
}

impl PendingProgressWriter {
    fn enqueue(&self, ledger: &ProgressLedger, checkpoint: bool) {
        let mut pending = match self.pending.lock() {
            Ok(pending) => pending,
            Err(error) => {
                warn!(error = %error, "progress writer state lock poisoned while enqueueing");
                return;
            }
        };
        pending.ledger = Some(ledger.clone());
        pending.checkpoint |= checkpoint;
        drop(pending);

        match self.signal.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {}
            Err(TrySendError::Disconnected(())) => {
                warn!("progress writer thread is unavailable");
            }
        }
    }
}

fn persist_pending_progress(workspace: &Path, pending: PendingProgress) {
    let Some(ledger) = pending.ledger else {
        return;
    };

    if let Err(error) = save_progress(workspace, &ledger.session_id, &ledger) {
        warn!(session = %ledger.session_id, error = %error, "failed to persist progress ledger");
    }

    if pending.checkpoint {
        let path = workspace.join("memories").join("progress.md");
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            warn!(path = %parent.display(), error = %error, "failed to create memories dir");
            return;
        }
        if let Err(error) = std::fs::write(&path, ledger.to_markdown()) {
            warn!(path = %path.display(), error = %error, "failed to write progress memory");
        }
    }
}

/// Observes and persists goal progress for a single session.
///
/// The monitor holds the domain state ([`ProgressLedger`]) and defers all IO to
/// an injected [`ProgressLedgerSink`], so the progress invariant logic is fully
/// testable in isolation.
pub struct ProgressMonitor {
    ledger: ProgressLedger,
    sink: Box<dyn ProgressLedgerSink>,
    /// Consecutive turns with no forward progress. Reset to 0 on advance.
    consecutive_stalls: u32,
}

impl ProgressMonitor {
    /// Create an in-memory monitor (no persistence) for `session_id`.
    #[must_use]
    pub fn new(session_id: &str, goal: &str) -> Self {
        Self::with_sink(ProgressLedger::new(session_id, goal), Box::new(NullProgressSink))
    }

    /// Create a monitor from an explicit ledger and sink (primary constructor
    /// for testing and custom persistence backends).
    #[must_use]
    pub fn with_sink(ledger: ProgressLedger, sink: Box<dyn ProgressLedgerSink>) -> Self {
        Self { ledger, sink, consecutive_stalls: 0 }
    }

    /// Create a monitor bound to a workspace, loading any previously persisted
    /// ledger so a resumed session continues from its real progress state.
    pub fn with_persistence(workspace: PathBuf, session_id: &str, goal: &str) -> Self {
        let ledger = load_progress(&workspace, session_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| ProgressLedger::new(session_id, goal));
        Self::with_sink(ledger, Box::new(SessionProgressSink::new(workspace)))
    }

    /// Create a persistent monitor without performing the initial ledger read
    /// on the Tokio worker running task setup.
    pub async fn with_persistence_async(workspace: PathBuf, session_id: &str, goal: &str) -> Self {
        let load_workspace = workspace.clone();
        let load_session_id = session_id.to_string();
        let fallback_goal = goal.to_string();
        let ledger = tokio::task::spawn_blocking(move || load_progress(&load_workspace, &load_session_id))
            .await
            .ok()
            .and_then(|result| result.ok())
            .flatten()
            .unwrap_or_else(|| ProgressLedger::new(session_id, &fallback_goal));
        Self::with_sink(ledger, Box::new(SessionProgressSink::new(workspace)))
    }

    /// Borrow the current ledger snapshot.
    #[must_use]
    pub fn ledger(&self) -> &ProgressLedger {
        &self.ledger
    }

    /// Whether the monitor is currently reporting a stall.
    #[must_use]
    pub fn is_stalled(&self) -> bool {
        self.ledger.is_stalled()
    }

    /// Fraction of milestones complete, `0.0..=1.0`.
    #[must_use]
    pub fn completion_ratio(&self) -> f32 {
        self.ledger.completion_ratio()
    }

    /// Whether all tracked milestones are complete (or none are tracked).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.ledger.is_complete()
    }

    /// Update the session goal and persist.
    pub fn set_goal(&mut self, goal: &str) {
        self.ledger.set_goal(goal);
        self.persist();
    }

    /// Replace the milestone set from the live task tracker/plan and persist.
    pub fn set_milestones(&mut self, milestones: Vec<Milestone>) {
        self.ledger.set_milestones(milestones);
        self.persist();
    }

    /// Record that this turn made forward progress (clears any stall).
    pub fn record_advance(&mut self) {
        self.ledger.note_advance();
        self.consecutive_stalls = 0;
        self.persist();
    }

    /// Record that this turn made no forward progress (may set a stall).
    pub fn record_stall(&mut self) {
        self.ledger.note_stall();
        self.consecutive_stalls = self.consecutive_stalls.saturating_add(1);
        self.persist();
    }

    /// Number of consecutive turns with no forward progress.
    /// Used by the context reset logic to decide when to trigger a reset.
    #[must_use]
    pub fn consecutive_stalls(&self) -> u32 {
        self.consecutive_stalls
    }

    fn persist(&self) {
        self.sink.persist(&self.ledger);
    }

    /// Flush a human-readable progress checkpoint through the sink (e.g. to
    /// `memories/progress.md`) so a resumed or forked session can re-ground on
    /// what is actually done without waiting for compaction. This is the
    /// proactive-context-grounding half of long-horizon support (the other half
    /// is the compaction-time [`ProgressLedger`]).
    ///
    /// Best-effort: for the default filesystem sink a write failure is logged,
    /// not fatal, and the [`NullProgressSink`] is a no-op.
    pub fn checkpoint(&self) {
        self.sink.checkpoint(&self.ledger);
    }
}

/// Map a free-form tracker status string onto a [`MilestoneStatus`].
#[must_use]
pub fn milestone_status_from_str(status: &str) -> MilestoneStatus {
    match status.trim().to_ascii_lowercase().as_str() {
        "done" | "complete" | "completed" | "pass" | "passed" | "success" => MilestoneStatus::Done,
        "blocked" | "stuck" | "waiting" => MilestoneStatus::Blocked,
        "in_progress" | "in progress" | "active" | "running" => MilestoneStatus::InProgress,
        _ => MilestoneStatus::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Sink that counts persist/checkpoint calls without touching disk.
    #[derive(Default)]
    struct CountingSink {
        persists: Arc<AtomicUsize>,
        checkpoints: Arc<AtomicUsize>,
    }

    impl ProgressLedgerSink for CountingSink {
        fn persist(&self, _ledger: &ProgressLedger) {
            self.persists.fetch_add(1, Ordering::Relaxed);
        }
        fn checkpoint(&self, _ledger: &ProgressLedger) {
            self.checkpoints.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn mutations_persist_through_injected_sink() {
        let persists = Arc::new(AtomicUsize::new(0));
        let checkpoints = Arc::new(AtomicUsize::new(0));
        let sink = CountingSink {
            persists: persists.clone(),
            checkpoints: checkpoints.clone(),
        };
        let mut monitor = ProgressMonitor::with_sink(ProgressLedger::new("s1", "goal"), Box::new(sink));

        monitor.set_milestones(vec![Milestone {
            id: "1".into(),
            description: "step".into(),
            status: MilestoneStatus::InProgress,
        }]);
        monitor.record_advance();
        monitor.record_stall();
        monitor.checkpoint();

        // set_milestones + record_advance + record_stall = 3 persists.
        assert_eq!(persists.load(Ordering::Relaxed), 3);
        assert_eq!(checkpoints.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn null_sink_monitor_is_pure_in_memory() {
        let mut monitor = ProgressMonitor::new("s2", "goal");
        assert!(monitor.is_complete()); // no milestones tracked yet
        monitor.set_milestones(vec![Milestone {
            id: "1".into(),
            description: "step".into(),
            status: MilestoneStatus::Pending,
        }]);
        assert!(!monitor.is_complete());
        assert!((monitor.completion_ratio() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn consecutive_stalls_increment_and_reset() {
        let mut monitor = ProgressMonitor::new("s3", "goal");
        assert_eq!(monitor.consecutive_stalls(), 0);

        monitor.record_stall();
        assert_eq!(monitor.consecutive_stalls(), 1);

        monitor.record_stall();
        assert_eq!(monitor.consecutive_stalls(), 2);

        // Advance resets the counter.
        monitor.record_advance();
        assert_eq!(monitor.consecutive_stalls(), 0);

        monitor.record_stall();
        assert_eq!(monitor.consecutive_stalls(), 1);
    }

    #[test]
    fn filesystem_writer_coalesces_updates_without_blocking_the_caller() {
        let (signal, receiver) = mpsc::sync_channel(1);
        let pending = Arc::new(Mutex::new(PendingProgress::default()));
        let writer = PendingProgressWriter { signal, pending: Arc::clone(&pending) };
        let first = ProgressLedger::new("first", "goal");
        let second = ProgressLedger::new("second", "goal");

        writer.enqueue(&first, false);
        writer.enqueue(&second, true);

        assert!(receiver.try_recv().is_ok());
        let pending = pending.lock().expect("pending state");
        assert_eq!(pending.ledger.as_ref().map(|ledger| ledger.session_id.as_str()), Some("second"));
        assert!(pending.checkpoint);
        assert!(receiver.try_recv().is_err(), "coalescing should keep one signal queued");
    }
}
