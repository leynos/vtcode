# VT Code Developer Guide

- [Development setup and workflows](./development/README.md)
- [Architecture](./ARCHITECTURE.md)
- [ACP runtime](./development/acp-runtime.md)
- [Custom-provider request policy](./development/CUSTOM_PROVIDER_REQUEST_POLICY.md)
- [Apply-patch safety contract](./development/patch-safety-contract.md)
- [Testing](./development/testing.md)
- [Configuration field reference](./config/CONFIG_FIELD_REFERENCE.md)

## Compaction architecture

Compaction is split between the core engine and the runtime adapters that
publish its result.

### Core engine (`vtcode-core::compaction`)

- `mod.rs` is the facade. It owns provider strategy selection, summary request
  construction, and the public `CompactionConfig`/`ManualCompactionOptions`
  surface.
- `context_budget.rs` owns model-window admission. It computes the effective
  history budget, refuses protected user instructions that cannot fit, and
  bounds the final compacted history.
- `history.rs` composes retained turns, summaries, and the continuity tail. It
  preserves the complete initial task and the latest steering messages, and
  normalizes provider-compacted output.
- `bounded_history.rs` limits optional payloads (tool previews) without
  breaking tool-call/result correlation.
- `protocol.rs` selects complete, correlatable protocol prefixes and the
  continuity tail.
- `auto.rs` is the single automatic-compaction entry point. It checks
  enablement, trigger, and suppression, produces a candidate, persists the
  recoverable artifact, and publishes the compacted history.
- `telemetry.rs` defines the bounded observation vocabulary and the
  `AutoCompactionObserver`/`MonotonicClock` ports. The process metric adapter
  lives in `vtcode-core::telemetry::compaction`, not in the compaction domain.

### Automatic-compaction candidate lifecycle

1. Eligibility: `auto_compaction_enabled`, token pressure, and suppression
   state are checked before any provider call.
2. Candidate: the engine selects provider-native or local summarization and
   builds a candidate history.
3. Persistence: the full pre-compaction history is written to the recoverable
   artifact store.
4. Publication: the candidate replaces live history only after the runtime's
   admission boundary accepts it.
5. Observation: attempt, terminal outcome, protected-budget refusal, and
   duration are emitted through the injected observer.

### ACP admission and publication

`vtcode-acp::zed::agent::compaction` treats the core result as a candidate.
`admission.rs` validates the complete resolved prompt (system/tool overhead and
output reservation) before replacing live history. The order is:

1. admit the candidate against the provider admission budget;
2. publish the accepted history to the thread;
3. checkpoint the session.

A refusal leaves history and checkpoint bytes untouched. Admission metrics are
separate from the engine's compaction result and per-session activity updates.
