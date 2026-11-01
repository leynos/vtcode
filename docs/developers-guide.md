# Diapason Developer Guide

- [Development setup and workflows](./development/README.md)
- [Architecture](./ARCHITECTURE.md)
- [ACP runtime](./development/acp-runtime.md)
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
  the write-ahead checkpoint must succeed before any tool runs.
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
