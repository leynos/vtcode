# VT Code Developer Guide

- [Development setup and workflows](./development/README.md)
- [Architecture](./ARCHITECTURE.md)
- [ACP runtime](./development/acp-runtime.md)
- [ACP protocol contracts](./acp/ACP_INTEGRATION.md)
- [ACP quick reference](./acp/ACP_QUICK_REFERENCE.md)
- [Custom-provider request policy](./development/CUSTOM_PROVIDER_REQUEST_POLICY.md)
- [Apply-patch safety contract](./development/patch-safety-contract.md)
- [Testing](./development/testing.md)
- [Configuration field reference](./config/CONFIG_FIELD_REFERENCE.md)

## ACP interrupted tool recovery

`crates/codegen/vtcode-acp/src/zed/agent/tool_recovery.rs` owns the history
repair shared by live turns and archive attach. Its contracts:

- `stage_tool_calls` writes one incomplete result per call before execution, and
  the write-ahead checkpoint must succeed before any tool runs. "Succeed" means
  the snapshot was persisted: a disabled or throttled history write fails the
  turn instead of running tools it could not recover.
- `replace_tool_results` replaces the latest staged placeholder for each reused
  call identifier, so a completed batch is never left partially resolved.
- `repair_unresolved_tool_calls` inserts placeholders for legacy archives,
  relocates a uniquely attributable late result, and refuses ambiguous
  duplicate or orphan evidence without mutating the archive.
- `RecoveryReport::history_changed` gates the follow-up checkpoint, so a repair
  that only moved a result is persisted as well as one that inserted a
  placeholder.

`vtcode-core` supplies the accompanying error contract: a `write_stdin` session
lost after the input write reports `partial_state_possible` through
`ExecSessionLostAfterInput`, while a pre-write resolution failure does not.
Design rationale and focused test commands are in the
[ACP runtime guide](./development/acp-runtime.md).

## ACP and Lody runtime

The [ACP runtime](./development/acp-runtime.md) guide describes the developer
surface behind the Zed integration:

- session-scoped subagent controller resolution, ownership, and descendant
  routing;
- lifecycle projection into standard ACP `session/update` messages;
- the `_lody/subagents/list`, `_lody/subagents/cancel`, and
  `_lody/subagents/output` management RPCs;
- `_lody/session/usage_update` publication; and
- the `supports_stream_usage` custom-provider opt-in.

The user-facing capability and extension contract is in the
[Zed ACP guide](./guides/zed-acp.md#acp-capability-negotiation-and-lody-extensions).
