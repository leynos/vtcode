# vtcode-exec-events

[Root AGENTS.md](../AGENTS.md) | Authoritative `ThreadEvent` contract. All
runtime events flow through this crate.

## Key Types

`ThreadEvent` enum — the single event type (serde-tagged) |
`VersionedThreadEvent` wrapper with schema version | `EventEmitter` trait |
`Usage` token accounting | `ThreadItem` + `ThreadItemDetails` item taxonomy |
`EVENT_SCHEMA_VERSION` semver string

## ThreadEvent Variants

`thread.started` | `thread.completed` | `thread.compact_boundary` |
`context.reset` | `turn.started` | `turn.completed` | `turn.failed` |
`turn.blocked` | `item.started` | `item.updated` | `item.completed` |
`plan.delta` | `plan.approval.requested` | `plan.approval.resolved` | `error`

## Rules

- **Do not invent parallel event types.** Extend `ThreadEvent` and
  `ThreadItemDetails` enums.
- `EVENT_SCHEMA_VERSION` must be bumped when the serialized contract changes.
- `EventEmitter` trait has a blanket `FnMut(&ThreadEvent)` impl.
- Feature-gated emitters: `telemetry-log` (LogEmitter), `telemetry-tracing`
  (TracingEmitter), `schema-export` (JSON Schema), `serde-json` (JSON helpers).
- `atif/` module exports ATIF (Agent Trace Interchange Format).
- `trace/` module implements Agent Trace spec for AI code attribution.

## Gotchas

- `vtcode-core::exec::events` re-exports these types — consumers should use
  that path, not depend on this crate directly.
- Plan approval state is represented by `PlanApprovalRequestedEvent` and
  `PlanApprovalResolvedEvent`; keep `PlanApprovalDecision` stable because it is
  consumed by headless clients and Open Responses adapters. Bounded failure
  explanations use the existing `ReasoningItem` with stage `"diagnosis"`; do
  not add a parallel event variant.
- `HarnessEventItem` uses `HarnessEventKind` enum — adding variants requires
  schema version bump.
- Schema `0.12.0` adds blocked-handoff resolution metadata; keep legacy
  payloads readable and ATIF output stable.
- Schema `0.13.0` adds `turn.blocked` plus `TurnBlocked`/
  `BlockedRecoveryStarted`/`BlockedRecoveryFinished` harness kinds;
  `turn.blocked` is emitted alongside `turn.failed` with fuse counters for UI
  subscribers.
