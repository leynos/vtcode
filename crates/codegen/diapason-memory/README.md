# diapason-memory

Unified per-session state store for Diapason — the single source of truth for
an agent session's state, context, and history.

Each workspace session is persisted under
`$XDG_STATE_HOME/diapason/workspaces/<workspace-hash>/sessions/<session_id>/`
(defaulting below `~/.local/state/diapason/` on Linux and BSD):

- `events.jsonl` — the canonical append-only `ThreadEvent` log (schema-versioned
  via `VersionedThreadEvent`). Everything else is derived from this.
- `manifest.json` — session metadata and counters.
- `index/turns.json` — byte-offset index enabling O(1) turn reconstruction.
- `derived/` — regenerated views (`trajectory.jsonl`, `memory.json`, …).

## Design

- **Append-only + off the hot path.** The live conversation stays in memory and
  is never reloaded from disk into context. Reads happen only for revert,
  compaction, analytics, and long-term-learning queries.
- **Single source of truth.** Checkpoints, trajectory metrics, and session
  memory are *derived* from `events.jsonl` rather than persisted independently,
  keeping workspace session events separate from repository-local `.diapason/`
  agent data.
- **Bounded growth.** `apply_retention` evicts the oldest/stale sessions so disk
  overhead does not accumulate across a long-lived agent.
- **Crash-safe + private.** Event compaction and metadata writes are atomic;
  session directories and artifacts use private permissions.

## Usage

```rust
use diapason_memory::{open, migrate_legacy, apply_retention, query_facts};

// Record a session's events (call from the runloop's event sink).
let log = open(workspace, session_id)?;
log.append(&event)?;
let turn = log.reconstruct_turn(3)?; // derived view, never into context

// One-off migration of the legacy overlapping stores.
let report = migrate_legacy(workspace, /* remove_legacy */ false)?;

// Bound growth and learn across sessions.
apply_retention(workspace, Default::default())?;
let facts = query_facts(workspace, 100)?;
```

## Modules

- `event_log` — append-only log, turn index, manifest.
- `migration` — import legacy history/trajectory stores.
- `retention` — retention policy + garbage collection.
- `query` — cross-session analytics and long-term-learning queries.
