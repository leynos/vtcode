# vtcode-acp

[Root AGENTS.md](../AGENTS.md) | Agent Client Protocol (Zed integration).
Canonical ACP entrypoint. Crate conventions and gotchas live in
[ACP Runtime](../../../docs/development/acp-runtime.md#crate-conventions-and-gotchas).

## Rules

- ACP 2.0.0 (schema 1.5.0) uses the SACP builder + handlers, not
  `impl acp::Agent`; `zed/agent/handlers.rs` is the canonical wiring.
- `ZedAgent` is `Send + Sync` for `cx.spawn`. Tool RPCs and `block_task()` are
  only safe inside a spawned task; calling them from a request handler
  deadlocks dispatch.
- Serialize `session/prompt` per session behind `SessionHandle::prompt_gate`
  (`Arc<tokio::sync::Mutex<()>>`), acquired only inside the spawned task after
  the unknown-session check; reset cancellation only after acquisition. Log
  `queued_prompts` before waiting and `wait_ms` after acquisition; observe
  steers, never cap them.
- A cancelled turn returns `StopReason::Cancelled`. A non-cancelled turn that
  would end with `EndTurn` needs assistant content or an explicit
  `internal_error` (`empty_response`, `tool_execution_failed`); never
  acknowledge a silent `EndTurn`. Persist any buffered text flushed visible on
  cancellation. Re-check cancellation after the turn's final checkpoint await,
  so a cancel that arrives during finalization still answers `Cancelled`.
- Provider-key resolution uses the configured `AuthCredentialsStoreMode`, not
  the platform default.
- `zed/agent/prompt_lifecycle_tests.rs` drives the duplex transport with a
  scripted provider; keep ordering deterministic (no sleeps).
