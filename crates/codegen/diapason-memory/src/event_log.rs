//! Append-only per-session `ThreadEvent` log plus index and manifest.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use diapason_commons::DiapasonPaths;
use diapason_exec_events::{EVENT_SCHEMA_VERSION, ThreadEvent, VersionedThreadEvent};
use serde::{Deserialize, Serialize};

use crate::error::SessionStoreError;
use crate::manifest::ManifestStore;
use crate::session_dir;

/// Default maximum number of events retained per session before the oldest
/// completed turns are evicted.
pub const DEFAULT_MAX_EVENTS: usize = 10_000;

/// Maximum serialized event bytes retained before an append forces a write.
/// Turn boundaries and reads still flush immediately.
const MAX_WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Minimal envelope used while rebuilding the turn index.
///
/// The index only needs the event discriminator. Deserializing a complete
/// [`VersionedThreadEvent`] here would allocate every nested tool argument,
/// output, and thread item even though none of that payload is retained.
#[derive(Debug, Deserialize)]
struct VersionedEventKind<'a> {
    #[serde(rename = "schema_version", borrow)]
    _schema_version: &'a str,
    #[serde(borrow)]
    event: EventKind<'a>,
}

#[derive(Debug, Deserialize)]
struct EventKind<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
}

/// Zero-clone serialization envelope for `ThreadEvent`.
///
/// Produces JSON byte-identical to `VersionedThreadEvent` but borrows the
/// event by reference instead of cloning it. `append` is called for every
/// runtime event, and `ThreadEvent` can carry large tool outputs / thread
/// items — cloning just to feed `serde_json::to_string` was pure waste.
#[derive(Serialize)]
struct BorrowedVersionedEvent<'a> {
    schema_version: &'a str,
    event: &'a ThreadEvent,
}

/// Turn-lifecycle discriminator extracted from either a `ThreadEvent` (at
/// append time) or a raw `&str` kind (during scan).  This is the single
/// representation that both code paths feed into
/// [`LogState::apply_lifecycle_event`], eliminating a duplicated state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleKind {
    ThreadStarted,
    ThreadCompleted,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    Other,
}

impl LifecycleKind {
    /// Discriminate from a runtime `ThreadEvent` at append time.
    #[inline]
    fn from_event(event: &ThreadEvent) -> Self {
        match event {
            ThreadEvent::ThreadStarted(_) => Self::ThreadStarted,
            ThreadEvent::ThreadCompleted(_) => Self::ThreadCompleted,
            ThreadEvent::TurnStarted(_) => Self::TurnStarted,
            ThreadEvent::TurnCompleted(_) => Self::TurnCompleted,
            ThreadEvent::TurnFailed(_) => Self::TurnFailed,
            _ => Self::Other,
        }
    }

    /// Discriminate from a raw event-type string at scan time.
    #[inline]
    fn from_kind(kind: &str) -> Self {
        match kind {
            "thread.started" => Self::ThreadStarted,
            "thread.completed" => Self::ThreadCompleted,
            "turn.started" => Self::TurnStarted,
            "turn.completed" => Self::TurnCompleted,
            "turn.failed" => Self::TurnFailed,
            _ => Self::Other,
        }
    }
}

/// In-memory state protected by a mutex (cheap; appends are infrequent relative
/// to model inference).
struct LogState {
    manifest: SessionManifest,
    index: TurnIndex,
    /// Whether we are currently inside a turn (between TurnStarted and
    /// TurnCompleted/TurnFailed). Used to update the last index entry's
    /// offsets as intermediate events arrive.
    in_turn: bool,
    /// Running byte offset of the next append. Avoids a `stat` syscall per
    /// event (the previous implementation re-statted the file twice on every
    /// `append`); initialized from the file length on `open`.
    next_offset: u64,
    /// Buffered pending writes to batch syscalls. Events are appended here
    /// and flushed to disk at turn boundaries or before read operations.
    write_buf: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct CapEvictionPlan {
    truncate_offset: u64,
    evicted_event_count: u64,
    evicted_turn_count: usize,
}

impl LogState {
    fn new(session_id: &str) -> Self {
        Self {
            manifest: SessionManifest::new(session_id),
            index: TurnIndex::default(),
            in_turn: false,
            next_offset: 0,
            write_buf: Vec::with_capacity(65536),
        }
    }

    /// Serialize `event` directly into the reusable write buffer with rollback
    /// on failure.
    ///
    /// This encapsulates the invariant that `write_buf` never contains a
    /// partial JSON document: if `serde_json::to_writer` fails mid-write the
    /// buffer is truncated back to its pre-serialization boundary.  Returns
    /// the `(start, end)` byte offsets of the serialized event so the caller
    /// can feed them to [`Self::apply_lifecycle_event`].
    fn serialize_event(&mut self, event: &ThreadEvent) -> Result<(u64, u64), SessionStoreError> {
        let start = self.next_offset;
        let buf_len_before = self.write_buf.len();
        if let Err(err) = serde_json::to_writer(
            &mut self.write_buf,
            &BorrowedVersionedEvent { schema_version: EVENT_SCHEMA_VERSION, event },
        ) {
            self.write_buf.truncate(buf_len_before);
            return Err(err.into());
        }
        self.write_buf.push(b'\n');
        let written = self.write_buf.len() - buf_len_before;
        let end = start + written as u64;
        self.next_offset = end;
        Ok((start, end))
    }

    /// Update the in-memory turn index and manifest counters for a single
    /// event.
    ///
    /// This is the single implementation of the turn-lifecycle state machine;
    /// both the append path (via [`LifecycleKind::from_event`]) and the scan
    /// path (via [`LifecycleKind::from_kind`]) route through here, eliminating
    /// a previously duplicated match block.
    ///
    /// Returns `true` when the event closes a turn boundary
    /// (`TurnCompleted` / `TurnFailed`) so the caller can persist metadata
    /// at the appropriate time (append persists immediately; scan persists
    /// once after the full scan).
    fn apply_lifecycle_event(&mut self, kind: LifecycleKind, start: u64, end: u64) -> bool {
        match kind {
            LifecycleKind::ThreadStarted => {
                self.manifest.status = "active".to_string();
                false
            }
            LifecycleKind::ThreadCompleted => {
                self.manifest.status = "completed".to_string();
                true
            }
            LifecycleKind::TurnStarted => {
                self.manifest.status = "active".to_string();
                self.in_turn = true;
                let n = self.manifest.turn_count + 1;
                self.index.entries.push_back(TurnIndexEntry {
                    turn_number: n,
                    start_offset: start,
                    end_offset: end,
                    event_count: 1,
                    ts: now_rfc3339(),
                });
                false
            }
            LifecycleKind::TurnCompleted | LifecycleKind::TurnFailed => {
                if self.in_turn {
                    if let Some(entry) = self.index.entries.back_mut() {
                        entry.end_offset = end;
                        entry.event_count += 1;
                    }
                    self.in_turn = false;
                    self.manifest.turn_count = self.index.entries.len() as u64;
                }
                true
            }
            LifecycleKind::Other => {
                if self.in_turn
                    && let Some(entry) = self.index.entries.back_mut()
                {
                    entry.end_offset = end;
                    entry.event_count += 1;
                }
                false
            }
        }
    }

    /// Plan a cap-enforcement eviction: pop the oldest completed turns from
    /// the index until `event_count` is within `max_events`.
    ///
    /// Returns the byte offset at which the file should be rewritten and the
    /// counts needed to apply the eviction after successful I/O. Returns
    /// `None` when no eviction is needed.
    fn plan_cap_eviction(&self, max_events: usize) -> Option<CapEvictionPlan> {
        if max_events == 0 || self.manifest.event_count <= max_events as u64 {
            return None;
        }
        let mut evicted_event_count = 0u64;
        let mut truncate_offset = 0u64;
        let mut evicted_turn_count = 0;
        for oldest in &self.index.entries {
            if self.manifest.event_count.saturating_sub(evicted_event_count) <= max_events as u64 {
                break;
            }
            truncate_offset = oldest.end_offset;
            evicted_event_count += oldest.event_count;
            evicted_turn_count += 1;
        }
        if truncate_offset == 0 || evicted_turn_count == 0 {
            None
        } else {
            Some(CapEvictionPlan {
                truncate_offset,
                evicted_event_count,
                evicted_turn_count,
            })
        }
    }

    fn apply_cap_eviction(&mut self, plan: CapEvictionPlan, next_offset: u64) {
        for _ in 0..plan.evicted_turn_count {
            let _ = self.index.entries.pop_front();
        }
        for entry in &mut self.index.entries {
            entry.start_offset = entry.start_offset.saturating_sub(plan.truncate_offset);
            entry.end_offset = entry.end_offset.saturating_sub(plan.truncate_offset);
        }
        self.next_offset = next_offset;
        self.manifest.event_count = self.manifest.event_count.saturating_sub(plan.evicted_event_count);
    }
}

/// Canonical append-only event log for a single session.
///
/// All session history is reconstructable from this log. Live conversation
/// state is never read back into context from here; the log is only consumed
/// for revert, compaction, analytics, and long-term-learning queries.
pub struct SessionEventLog {
    events_path: PathBuf,
    manifest_store: ManifestStore,
    file: Mutex<Option<File>>,
    state: Mutex<LogState>,
    max_events: usize,
}

impl SessionEventLog {
    /// Open the log for `session_id`, creating the session directory tree and
    /// rebuilding the index from `events.jsonl` if it already exists.
    pub(crate) fn open(workspace: &Path, session_id: &str, max_events: usize) -> Result<Self, SessionStoreError> {
        let dir = session_dir(workspace, session_id)?;
        crate::ensure_private_directory(&crate::sessions_root(workspace)?)?;
        crate::ensure_private_directory(&dir)?;
        crate::ensure_private_directory(&dir.join(crate::DERIVED_DIR))?;
        crate::ensure_private_directory(&dir.join("index"))?;
        let events_path = dir.join("events.jsonl");
        let file = DiapasonPaths::open_private_append_file(&events_path)
            .map_err(|error| SessionStoreError::io(events_path.clone(), std::io::Error::other(error)))?;
        let manifest_store = ManifestStore::new(dir.clone());
        let log = Self {
            events_path: events_path.clone(),
            manifest_store,
            file: Mutex::new(Some(file)),
            state: Mutex::new(LogState::new(session_id)),
            max_events,
        };
        // Try the fast path: read the persisted manifest + index and skip
        // the O(n) scan when they are present and consistent.
        let manifest_opt = log.manifest_store.load_manifest()?;
        let index_opt = log.manifest_store.load_turn_index()?;
        let file_len = log.event_file_metadata_len()?;
        match (manifest_opt, index_opt) {
            (Some(manifest), Some(index)) if index.is_valid_for_file(file_len) => {
                let mut st = log.state.lock().map_err(poison)?;
                st.manifest = manifest;
                st.index = index;
                st.next_offset = file_len;
            }
            _ => {
                log.scan()?;
                let mut st = log.state.lock().map_err(poison)?;
                st.next_offset = file_len;
                log.persist_meta_locked(&mut st)?;
            }
        }
        Ok(log)
    }

    /// Append an event to the log and update the in-memory index/manifest.
    pub fn append(&self, event: &ThreadEvent) -> Result<(), SessionStoreError> {
        let mut st = self.state.lock().map_err(poison)?;

        // Serialize into the write buffer with rollback on failure — the
        // invariant that `write_buf` never contains partial JSON is
        // encapsulated in `serialize_event`.
        let (start, end) = st.serialize_event(event)?;

        st.manifest.event_count += 1;
        st.manifest.updated_at = now_rfc3339();

        // Route through the single turn-lifecycle state machine.  When the
        // event closes a turn, persist metadata immediately so a reopen
        // after a mid-turn crash sees a consistent index.
        let is_turn_boundary = st.apply_lifecycle_event(LifecycleKind::from_event(event), start, end);
        if is_turn_boundary {
            self.persist_meta_locked(&mut st)?;
        }

        if st.write_buf.len() >= MAX_WRITE_BUFFER_BYTES {
            // Persist metadata with the bounded byte flush so a reopen after
            // a mid-turn crash does not trust an index that predates these
            // already-written events.
            self.persist_meta_locked(&mut st)?;
        }
        drop(st);
        self.enforce_event_cap()
    }

    /// Enforce the per-session event cap by evicting the oldest completed
    /// turns when the log exceeds [`Self::max_events`]. Returns `Ok(())` even
    /// when no truncation is needed or the cap is disabled (`max_events == 0`).
    fn enforce_event_cap(&self) -> Result<(), SessionStoreError> {
        let mut st = self.state.lock().map_err(poison)?;

        // `plan_cap_eviction` encapsulates the index arithmetic and returns
        // `None` when the cap is disabled or not yet exceeded.
        let Some(plan) = st.plan_cap_eviction(self.max_events) else {
            return Ok(());
        };

        // Keep ordinary appends in memory until a turn boundary or an
        // explicit read. Cap enforcement is the one append-time path that
        // needs the complete on-disk file before rewriting it.
        self.flush_write_buf_locked(&mut st)?;

        let remaining = {
            let mut file_slot = self.file.lock().map_err(poison)?;
            let file = file_slot.as_mut().ok_or_else(|| self.event_file_unavailable())?;
            let file_len = file
                .metadata()
                .map_err(|error| SessionStoreError::io(&self.events_path, error))?
                .len();
            if plan.truncate_offset > file_len {
                return Err(SessionStoreError::io(
                    &self.events_path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "cap offset exceeds event log length"),
                ));
            }
            file.seek(SeekFrom::Start(plan.truncate_offset))
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            let mut remaining = Vec::new();
            file.read_to_end(&mut remaining)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            remaining
        };

        let next_offset = self.replace_event_file_contents(&remaining)?;
        st.apply_cap_eviction(plan, next_offset);
        // The rewrite changed byte offsets and retained counts; persist the
        // derived metadata before exposing the append as successful.
        self.persist_meta_locked(&mut st)?;
        Ok(())
    }

    /// Reconstruct every event belonging to `turn`.
    pub(crate) fn reconstruct_turn(&self, turn: u64) -> Result<Vec<ThreadEvent>, SessionStoreError> {
        let entry = {
            let st = self.state.lock().map_err(poison)?;
            st.index
                .entries
                .iter()
                .find(|e| e.turn_number == turn)
                .cloned()
                .ok_or(SessionStoreError::TurnNotFound { session: st.manifest.session_id.clone(), turn })?
        };
        {
            let mut st = self.state.lock().map_err(poison)?;
            self.flush_write_buf_locked(&mut st)?;
        }
        let buf = {
            let mut file_slot = self.file.lock().map_err(poison)?;
            let file = file_slot.as_mut().ok_or_else(|| self.event_file_unavailable())?;
            file.seek(SeekFrom::Start(entry.start_offset))
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            let len = usize::try_from(entry.end_offset.checked_sub(entry.start_offset).ok_or_else(|| {
                SessionStoreError::io(
                    &self.events_path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "turn index offsets are out of order"),
                )
            })?)
            .map_err(|error| {
                SessionStoreError::io(&self.events_path, std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })?;
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            buf
        };
        let text = String::from_utf8_lossy(&buf);
        let mut events = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // The index scan only validates the event envelope (plus the
            // lifecycle shape) so it can rebuild cheaply. A line accepted by
            // the scan can therefore still fail full decoding here; skip it
            // instead of failing the whole reconstruction (revert, compaction,
            // and analytics must not break on a single malformed record).
            let v: VersionedThreadEvent = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            events.push(v.into_event());
        }
        Ok(events)
    }

    /// Number of turns recorded.
    #[must_use]
    pub(crate) fn turn_count(&self) -> u64 {
        self.state.lock().map_err(poison).map_or(0, |s| s.manifest.turn_count)
    }

    /// Number of events recorded.
    #[must_use]
    pub fn event_count(&self) -> u64 {
        self.state.lock().map_err(poison).map_or(0, |s| s.manifest.event_count)
    }

    /// Flush pending event bytes and metadata to the session store.
    pub fn flush(&self) -> Result<(), SessionStoreError> {
        let mut st = self.state.lock().map_err(poison)?;
        self.persist_meta_locked(&mut st)
    }

    /// Snapshot of the session manifest.
    #[must_use]
    pub fn manifest(&self) -> SessionManifest {
        self.state
            .lock()
            .map_err(poison)
            .map(|s| s.manifest.clone())
            .unwrap_or_else(|_| SessionManifest::new(""))
    }

    /// Snapshot of the turn index.
    #[must_use]
    pub fn turn_index(&self) -> TurnIndex {
        self.state.lock().map_err(poison).map(|s| s.index.clone()).unwrap_or_default()
    }

    /// Flush metadata for callers that explicitly close a log handle.
    ///
    /// Terminal status is intentionally controlled only by a persisted
    /// `thread.completed` event. This method does not synthesize lifecycle
    /// state for callers that merely release a store handle.
    pub(crate) fn complete(&self) -> Result<(), SessionStoreError> {
        let mut st = self.state.lock().map_err(poison)?;
        st.manifest.updated_at = now_rfc3339();
        self.persist_meta_locked(&mut st)
    }

    /// Rebuild index + manifest by scanning `events.jsonl` (authoritative).
    ///
    /// Reads the file line-by-line via `BufReader` to avoid loading the entire
    /// log into memory. Long-lived sessions can otherwise produce multi-megabyte
    /// logs that spike memory on every reopen.
    fn scan(&self) -> Result<(), SessionStoreError> {
        let mut st = self.state.lock().map_err(poison)?;
        let file = self
            .file
            .lock()
            .map_err(poison)?
            .as_ref()
            .ok_or_else(|| self.event_file_unavailable())?
            .try_clone()
            .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
        let mut reader = std::io::BufReader::new(file);
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
        let mut buf = Vec::new();
        let mut pos = 0u64;
        let mut first_ts: Option<String> = None;
        loop {
            buf.clear();
            let n = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            if n == 0 {
                break;
            }
            let line_end = pos + n as u64;
            let trimmed = std::str::from_utf8(&buf).unwrap_or("").trim();
            if !trimmed.is_empty()
                && let Ok(v) = serde_json::from_str::<VersionedEventKind<'_>>(trimmed)
            {
                let kind = v.event.kind;
                if requires_full_lifecycle_validation(kind)
                    && serde_json::from_str::<VersionedThreadEvent>(trimmed).is_err()
                {
                    pos = line_end;
                    continue;
                }
                st.manifest.event_count += 1;
                // `thread.started` is not part of the turn lifecycle — it
                // only seeds `created_at` on the first occurrence.
                if kind == "thread.started" && first_ts.is_none() {
                    first_ts = Some(now_rfc3339());
                }
                // Route turn-lifecycle events through the same state machine
                // as `append`, eliminating a previously duplicated match block.
                st.apply_lifecycle_event(LifecycleKind::from_kind(kind), pos, line_end);
            }
            pos = line_end;
        }
        // The scan uses `LogState.in_turn` via `apply_lifecycle_event`; reset
        // it so a reopen that ends mid-turn does not leave the state machine
        // in the "inside a turn" position (the fast path also starts with
        // `in_turn = false`).
        st.in_turn = false;
        if let Some(ts) = first_ts
            && st.manifest.created_at.is_empty()
        {
            st.manifest.created_at = ts;
        }
        Ok(())
    }

    fn persist_meta_locked(&self, st: &mut LogState) -> Result<(), SessionStoreError> {
        self.flush_write_buf_locked(st)?;
        self.manifest_store.write_manifest(&st.manifest)?;
        self.manifest_store.write_turn_index(&st.index)?;
        Ok(())
    }

    /// Flush the in-memory write buffer to the underlying file.
    fn flush_write_buf_locked(&self, st: &mut LogState) -> Result<(), SessionStoreError> {
        if st.write_buf.is_empty() {
            return Ok(());
        }
        let mut file_slot = self.file.lock().map_err(poison)?;
        let file = file_slot.as_mut().ok_or_else(|| self.event_file_unavailable())?;
        let previous_len = file.metadata().map_err(|e| SessionStoreError::io(&self.events_path, e))?.len();
        if let Err(error) = file.write_all(&st.write_buf) {
            if file.set_len(previous_len).is_err() {
                st.write_buf.clear();
            }
            return Err(SessionStoreError::io(&self.events_path, error));
        }
        if let Err(error) = file.sync_data() {
            st.write_buf.clear();
            return Err(SessionStoreError::io(&self.events_path, error));
        }
        st.write_buf.clear();
        Ok(())
    }

    fn event_file_metadata_len(&self) -> Result<u64, SessionStoreError> {
        let file_slot = self.file.lock().map_err(poison)?;
        file_slot
            .as_ref()
            .ok_or_else(|| self.event_file_unavailable())?
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| SessionStoreError::io(&self.events_path, error))
    }

    fn open_event_file(&self) -> Result<File, SessionStoreError> {
        DiapasonPaths::open_private_append_file(&self.events_path)
            .map_err(|error| SessionStoreError::io(&self.events_path, std::io::Error::other(error)))
    }

    fn replace_event_file_contents(&self, contents: &[u8]) -> Result<u64, SessionStoreError> {
        let old_file = {
            let mut file_slot = self.file.lock().map_err(poison)?;
            file_slot.take().ok_or_else(|| self.event_file_unavailable())?
        };
        drop(old_file);

        if let Err(error) = DiapasonPaths::write_private_file_atomic(&self.events_path, contents)
            .map_err(|error| SessionStoreError::io(&self.events_path, std::io::Error::other(error)))
        {
            let restored = self.open_event_file();
            if let Ok(file) = restored {
                let mut file_slot = self.file.lock().map_err(poison)?;
                *file_slot = Some(file);
                return Err(error);
            }
            return Err(SessionStoreError::io(
                &self.events_path,
                std::io::Error::other(format!("{error}; failed to restore event log handle")),
            ));
        }

        let replacement = self.open_event_file()?;
        let next_offset = replacement
            .metadata()
            .map_err(|error| SessionStoreError::io(&self.events_path, error))?
            .len();
        let mut file_slot = self.file.lock().map_err(poison)?;
        *file_slot = Some(replacement);
        Ok(next_offset)
    }

    fn event_file_unavailable(&self) -> SessionStoreError {
        SessionStoreError::io(&self.events_path, std::io::Error::other("event log file is unavailable"))
    }
}

fn requires_full_lifecycle_validation(kind: &str) -> bool {
    matches!(kind, "thread.started" | "thread.completed" | "turn.started" | "turn.completed" | "turn.failed")
}

impl Drop for SessionEventLog {
    fn drop(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            // The fallible `flush` method is the authoritative shutdown path;
            // Drop only provides a best-effort byte flush for callers that do
            // not explicitly close the log. Rewriting metadata here could
            // overwrite a manifest update made by another owner after the
            // last append.
            let _ = self.flush_write_buf_locked(&mut st);
        }
    }
}

/// Locate the next newline at or after `from`, returning a past-the-end index.
fn poison<T>(_e: std::sync::PoisonError<T>) -> SessionStoreError {
    SessionStoreError::Io {
        path: PathBuf::new(),
        source: std::io::Error::other("session store lock poisoned"),
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// Session-level metadata persisted to `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionManifest {
    /// Stable session identifier (directory name).
    pub session_id: String,
    /// Layout schema version (`SESSION_STORE_SCHEMA_VERSION`).
    schema_version: u32,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
    /// Number of completed turns.
    pub turn_count: u64,
    /// Total number of events recorded.
    pub event_count: u64,
    /// Lifecycle status (`active` | `completed`).
    pub status: String,
}

impl SessionManifest {
    /// Create a fresh manifest for a session.
    #[must_use]
    pub(crate) fn new(session_id: &str) -> Self {
        let ts = now_rfc3339();
        Self {
            session_id: session_id.to_string(),
            schema_version: crate::SESSION_STORE_SCHEMA_VERSION,
            created_at: ts.clone(),
            updated_at: ts,
            turn_count: 0,
            event_count: 0,
            status: "active".to_string(),
        }
    }
}

/// Byte-offset index of a single turn within `events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnIndexEntry {
    /// Turn ordinal (1-based).
    turn_number: u64,
    /// Byte offset of the turn's first event.
    start_offset: u64,
    /// Byte offset just past the turn's last event.
    end_offset: u64,
    /// Number of events in the turn.
    event_count: u64,
    /// RFC3339 timestamp of turn start.
    ts: String,
}

/// Ordered index of all turns in a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnIndex {
    /// Turn entries in ordinal order.
    entries: VecDeque<TurnIndexEntry>,
}

impl TurnIndex {
    /// Number of indexed turns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn is_valid_for_file(&self, file_len: u64) -> bool {
        let mut previous_end = 0u64;
        self.entries.iter().all(|entry| {
            let valid = entry.event_count > 0
                && entry.start_offset >= previous_end
                && entry.start_offset <= entry.end_offset
                && entry.end_offset <= file_len;
            if valid {
                previous_end = entry.end_offset;
            }
            valid
        })
    }
}

#[cfg(test)]
mod borrowed_envelope_tests {
    use super::{BorrowedVersionedEvent, EVENT_SCHEMA_VERSION};
    use diapason_exec_events::{
        ThreadEvent, ThreadStartedEvent, TurnCompletedEvent, TurnStartedEvent, Usage, VersionedThreadEvent,
    };

    /// The borrowed envelope must produce JSON byte-identical to
    /// `VersionedThreadEvent::new(event.clone())`. This guards against drift if
    /// either the envelope or the canonical wrapper is modified.
    #[test]
    fn borrowed_envelope_matches_versioned_envelope() {
        for event in [
            ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }),
            ThreadEvent::TurnStarted(TurnStartedEvent::default()),
            ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ] {
            let canonical =
                serde_json::to_string(&VersionedThreadEvent::new(event.clone())).expect("canonical serialize");
            let borrowed = serde_json::to_string(&BorrowedVersionedEvent {
                schema_version: EVENT_SCHEMA_VERSION,
                event: &event,
            })
            .expect("borrowed serialize");
            assert_eq!(canonical, borrowed, "JSON differs for {event:?}");
        }
    }
}

#[cfg(test)]
mod lifecycle_state_machine_tests {
    use super::{LifecycleKind, LogState};
    use diapason_exec_events::{
        ThreadCompletedEvent, ThreadCompletionSubtype, ThreadEvent, ThreadStartedEvent, TurnCompletedEvent,
        TurnFailedEvent, TurnStartedEvent, Usage,
    };

    fn fresh_state() -> LogState {
        LogState::new("test-session")
    }

    #[test]
    fn lifecycle_kind_from_event_covers_all_variants() {
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::TurnStarted(TurnStartedEvent::default())),
            LifecycleKind::TurnStarted
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() })),
            LifecycleKind::TurnCompleted
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::TurnFailed(TurnFailedEvent {
                message: "err".to_string(),
                usage: None,
            })),
            LifecycleKind::TurnFailed
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "x".to_string() })),
            LifecycleKind::ThreadStarted
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::ThreadCompleted(ThreadCompletedEvent {
                thread_id: "x".to_string(),
                session_id: "x".to_string(),
                subtype: ThreadCompletionSubtype::Success,
                outcome_code: "completed".to_string(),
                result: None,
                stop_reason: None,
                usage: Usage::default(),
                total_cost_usd: None,
                num_turns: 1,
            })),
            LifecycleKind::ThreadCompleted
        );
        assert_eq!(LifecycleKind::from_kind("thread.started"), LifecycleKind::ThreadStarted);
        assert_eq!(LifecycleKind::from_kind("thread.completed"), LifecycleKind::ThreadCompleted);
    }

    #[test]
    fn lifecycle_kind_from_str_matches_event_discriminator() {
        assert_eq!(LifecycleKind::from_kind("turn.started"), LifecycleKind::TurnStarted);
        assert_eq!(LifecycleKind::from_kind("turn.completed"), LifecycleKind::TurnCompleted);
        assert_eq!(LifecycleKind::from_kind("turn.failed"), LifecycleKind::TurnFailed);
        assert_eq!(LifecycleKind::from_kind("tool.called"), LifecycleKind::Other);
        assert_eq!(LifecycleKind::from_kind("thread.started"), LifecycleKind::ThreadStarted);
        assert_eq!(LifecycleKind::from_kind("thread.completed"), LifecycleKind::ThreadCompleted);
    }

    #[test]
    fn turn_started_pushes_index_entry_and_sets_in_turn() {
        let mut st = fresh_state();
        st.manifest.status = "completed".to_string();
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        assert!(!is_boundary, "TurnStarted is not a turn boundary");
        assert!(st.in_turn);
        assert_eq!(st.manifest.status, "active");
        assert_eq!(st.index.entries.len(), 1);
        let entry = &st.index.entries[0];
        assert_eq!(entry.turn_number, 1);
        assert_eq!(entry.start_offset, 0);
        assert_eq!(entry.end_offset, 100);
        assert_eq!(entry.event_count, 1);
    }

    #[test]
    fn intermediate_events_extend_current_turn() {
        let mut st = fresh_state();
        st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        // Simulate two intermediate events.
        let is_b1 = st.apply_lifecycle_event(LifecycleKind::Other, 100, 200);
        let is_b2 = st.apply_lifecycle_event(LifecycleKind::Other, 200, 300);
        assert!(!is_b1 && !is_b2);
        assert!(st.in_turn);
        assert_eq!(st.index.entries.len(), 1);
        let entry = &st.index.entries[0];
        assert_eq!(entry.end_offset, 300);
        assert_eq!(entry.event_count, 3);
    }

    #[test]
    fn turn_completed_closes_turn_and_returns_boundary() {
        let mut st = fresh_state();
        st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        st.apply_lifecycle_event(LifecycleKind::Other, 100, 200);
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnCompleted, 200, 300);
        assert!(is_boundary);
        assert!(!st.in_turn);
        assert_eq!(st.manifest.turn_count, 1);
        assert_eq!(st.manifest.status, "active");
        st.apply_lifecycle_event(LifecycleKind::ThreadCompleted, 300, 400);
        assert_eq!(st.manifest.status, "completed");
        let entry = &st.index.entries[0];
        assert_eq!(entry.end_offset, 300);
        assert_eq!(entry.event_count, 3);
    }

    #[test]
    fn turn_failed_closes_turn_without_terminal_thread_status() {
        let mut st = fresh_state();
        st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnFailed, 100, 200);
        assert!(is_boundary);
        assert!(!st.in_turn);
        assert_eq!(st.manifest.turn_count, 1);
        assert_eq!(st.manifest.status, "active");
        st.apply_lifecycle_event(LifecycleKind::ThreadCompleted, 200, 300);
        assert_eq!(st.manifest.status, "completed");
    }

    #[test]
    fn turn_completed_without_turn_started_is_idempotent() {
        let mut st = fresh_state();
        // Receiving TurnCompleted without a preceding TurnStarted should not
        // panic or corrupt the index; terminal status remains active until the
        // thread lifecycle itself completes.
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnCompleted, 0, 100);
        assert!(is_boundary);
        assert!(!st.in_turn);
        assert_eq!(st.manifest.turn_count, 0, "no turn was started");
        assert_eq!(st.manifest.status, "active");
        st.apply_lifecycle_event(LifecycleKind::ThreadCompleted, 100, 200);
        assert_eq!(st.manifest.status, "completed");
        assert!(st.index.entries.is_empty());
    }

    #[test]
    fn multiple_turns_get_incrementing_ordinals() {
        let mut st = fresh_state();
        for n in 1..=3 {
            st.apply_lifecycle_event(LifecycleKind::TurnStarted, n * 100, n * 100 + 50);
            st.apply_lifecycle_event(LifecycleKind::TurnCompleted, n * 100 + 50, n * 100 + 100);
        }
        assert_eq!(st.index.entries.len(), 3);
        for (i, entry) in st.index.entries.iter().enumerate() {
            assert_eq!(entry.turn_number, (i + 1) as u64);
        }
        assert_eq!(st.manifest.turn_count, 3);
    }
}

#[cfg(test)]
mod cap_eviction_tests {
    use super::{LogState, TurnIndexEntry};

    /// Build a `LogState` with `turns` fake turns, each having `events_per_turn`
    /// events, starting at byte offset 0.
    fn state_with_turns(turns: usize, events_per_turn: u64) -> LogState {
        let mut st = LogState::new("cap-test");
        st.manifest.event_count = (turns as u64) * events_per_turn;
        let mut offset = 0u64;
        for n in 1..=turns {
            st.index.entries.push_back(TurnIndexEntry {
                turn_number: n as u64,
                start_offset: offset,
                end_offset: offset + events_per_turn * 10,
                event_count: events_per_turn,
                ts: "2026-01-01T00:00:00Z".to_string(),
            });
            offset += events_per_turn * 10;
        }
        st
    }

    #[test]
    fn no_eviction_when_under_cap() {
        let st = state_with_turns(3, 2); // 6 events
        assert!(st.plan_cap_eviction(10).is_none());
        assert_eq!(st.index.entries.len(), 3, "no turns should be evicted");
    }

    #[test]
    fn no_eviction_when_cap_disabled() {
        let st = state_with_turns(5, 2); // 10 events
        assert!(st.plan_cap_eviction(0).is_none());
        assert_eq!(st.index.entries.len(), 5);
    }

    #[test]
    fn evicts_oldest_turns_to_meet_cap() {
        // 5 turns × 2 events = 10 events; cap = 6 → need to evict 2 turns (4 events).
        let st = state_with_turns(5, 2);
        let plan = st.plan_cap_eviction(6).expect("eviction planned");
        assert_eq!(plan.evicted_event_count, 4, "should evict 4 events (2 turns)");
        assert_eq!(st.index.entries.len(), 5, "planning must not mutate state");
        // Truncate offset is the end of the last evicted turn.
        assert_eq!(plan.truncate_offset, 40); // 2 turns × 20 bytes each

        // Applying the plan leaves turns 3, 4, 5.
        let mut st = st;
        st.apply_cap_eviction(plan, 60);
        assert_eq!(st.index.entries.len(), 3, "should keep 3 turns");
        assert_eq!(st.index.entries[0].turn_number, 3);
        assert_eq!(st.index.entries[2].turn_number, 5);
    }

    #[test]
    fn evicts_all_turns_when_cap_smaller_than_one_turn() {
        // 3 turns × 5 events = 15 events; cap = 3 → evict turns until ≤ 3 remain.
        // Each turn has 5 events, so evicting 2 turns leaves 5 (>3), evicting
        // 3 turns leaves 0.
        let st = state_with_turns(3, 5);
        let plan = st.plan_cap_eviction(3).expect("eviction planned");
        assert_eq!(plan.evicted_event_count, 15, "all events evicted");
        let mut st = st;
        st.apply_cap_eviction(plan, 0);
        assert_eq!(st.index.entries.len(), 0);
    }
}
