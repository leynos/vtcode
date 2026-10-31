# Diapason Developer Guide

- [Development setup and workflows](./development/README.md)
- [Architecture](./ARCHITECTURE.md)
- [ACP runtime](./development/acp-runtime.md)
- [ACP protocol contracts](./acp/ACP_INTEGRATION.md)
- [ACP quick reference](./acp/ACP_QUICK_REFERENCE.md)
- [Custom-provider request policy](./development/CUSTOM_PROVIDER_REQUEST_POLICY.md)
- [Apply-patch safety contract](./development/patch-safety-contract.md)
- [Diapason shell-policy proposal](./rfcs/rfc-0001-diapason-shell-policy.md):
  a proposal that changes no runtime APIs or dependencies. The
  [technical design](./design/diapason-shell-policy-design.md) is the source
  for the architecture; the
  [roadmap](./project/diapason-shell-policy-roadmap.md) tracks feasibility and
  delivery.
- [Testing](./development/testing.md)
- [Instruction discovery boundary](./development/instruction-discovery.md)
- [Configuration field reference](./config/CONFIG_FIELD_REFERENCE.md)
- [Documentation style guide](./documentation-style-guide.md)
- [0.157.0 release notes](./releases/0.157.0.md) - Breaking API renames and
  migration actions.

## ACP interrupted tool recovery

`crates/codegen/diapason-acp/src/zed/agent/tool_recovery.rs` owns the history
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

`diapason-core` supplies the accompanying error contract: a `write_stdin`
session lost after the input write reports `partial_state_possible` through
`ExecSessionLostAfterInput`, while a pre-write resolution failure does not.
Design rationale and focused test commands are in the
[ACP runtime guide](./development/acp-runtime.md).

### History mutation and turn admission

`ThreadRuntimeHandle::mutate_messages` applies a short, synchronous callback
while holding the session lock. The callback must not call back into the
handle, perform I/O, or await; that keeps placeholder staging and result
replacement atomic without creating a second owner of session state. Use
`replace_messages` only when the new history does not depend on the current one.

`TurnGuard::begin` claims the thread's single in-flight turn before a prompt
mutates history. A concurrent prompt is rejected with `turn_in_progress`
instead of interleaving tool results with the turn that already owns the
thread. The guard releases the slot on drop, including on an error path.

### Archive registration boundary

`session/load` and `session/resume` repair the archive, then checkpoint the
repaired history with `CheckpointDurability::Required`, and only then register
the session. A failed repair checkpoint releases the workspace lease and
returns an error, so a partially repaired session never becomes visible to a
prompt or tool call. Ambiguous history is rejected before any archive write.

### Stale exec-session error contract

`ExecSessionNotFound` and `ExecSessionLostAfterInput` are typed causes in
`diapason-core`. `ToolExecutionError::from_anyhow` maps both to a non-retryable
`ResourceNotFound` classification and records a typed `stale_exec_session`
state on the error. Policy decisions read that typed state; the client-facing
`details` payload (`reason: "stale_exec_session"`, `session_id`, and
`input_delivered` when applicable) is rendered only when the error is
serialized. Do not parse `details` to decide recovery behaviour. The JSON
envelope carries the typed `stale_exec_session` field alongside `details`, so
`ToolExecutionError::from_error_payload` restores policy state through serde
when the retry loop parses a tool failure back into a structured error.

Related design records and protocol references:

- [ACP interrupted tool recovery ExecPlan](./execplans/acp-unresolved-tool-recovery.md)
- [ACP PR convergence ExecPlan](./execplans/review-acp-pr13-convergence-20260908.md)
- [Zed ACP guide](./guides/zed-acp.md)
- [Tool specifications](./tools/TOOL_SPECS.md)

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
