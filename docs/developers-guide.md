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
  migration actions, including the `catalog` to `catalogue` Rust API rename and
  the external spellings that remain unchanged.

## Catalogue naming

The naming and compatibility decisions are recorded in
[Tool catalogue naming and compatibility](./design/tool-catalogue-naming.md).

Rust APIs use the native `catalogue` spelling: `ToolCatalogue`,
`ToolCatalogueEntry`, `ToolCatalogueSource`, `CatalogueToolKind`,
`SessionToolCatalogue`, `SessionToolCatalogueSnapshot`,
`SessionToolCatalogueState`, `PodCatalogue`, `ModelCatalogueEntry`,
`MergeCatalogue*`, `MergeGatewayCatalogueClient`, `MergeModelCatalogueResponse`,
`PodsStore::catalogue_path`, `PodsStore::load_catalogue`,
`PodsStore::save_catalogue`, `catalogue_tools`, `model_catalogue_entry`,
`catalogue_provider_keys`, `is_model_catalogue_eligible`,
`validate_prompt_catalogue_alignment`, `ToolRegistry::tool_catalogue_state`,
`ToolRegistration::with_catalogue_source`, `ToolRegistration::catalogue_source`,
`SessionRequestEnvelope::catalogue_hash`,
`HarnessRequestPlan::tool_catalogue_hash`,
`HarnessRequestPlanInput::tool_catalogue_hash`,
`SegmentBoundaryReason::ToolCatalogueEpoch`, `ResolvedModel::catalogue`, and the
`agent::tool_catalogue`, `handlers::session_tool_catalogue`,
`providers::merge_gateway_catalogue`, `tool_catalogue_facade` and
`pods::catalogue` modules. The old spellings are removed rather than
deprecated, so in-repository callers must use the new names; the full
removed/replacement list is in the
[0.157.0 release notes](./releases/0.157.0.md#catalogue-spelling-renames).

Compatibility applies only to spellings that leave the Rust API:

- Configuration keys, wire protocol fields and telemetry metric names keep
  their canonical `catalog` spelling. Rename the Rust item, not the serialized
  name, and keep an explicit `#[serde(rename = "...")]` where a field moves.
- The pod store keeps reading and writing `catalog.json`. Only the accessor
  name changes, so existing deployment profiles continue to load.
- Runtime prompt metadata uses the `[Runtime Tool Catalogue]` header.
  Refreshing a prompt removes a legacy `[Runtime Tool Catalog]` section while
  preserving notices appended after it, and `stable_system_prefix_hash` ignores
  metadata under either header.

Do not add aliases for the old Rust names, and do not rewrite persisted data to
match the new spelling.

## ACP interrupted tool recovery

`crates/codegen/diapason-acp/src/zed/agent/tool_recovery.rs` owns the history
repair shared by live turns and archive attach. Its contracts:

- `stage_tool_calls` writes one incomplete result per call before execution, and
  the write-ahead checkpoint must succeed before any tool runs. Persistence
  alone is not success: a disabled or throttled history write fails the turn
  instead of running tools it could not recover.
- A required checkpoint also verifies retention. A compacted payload that drops
  the staged tool batch, or any evidence a repair just made durable, is
  rejected before it replaces the archive, and the persisted file at the exact
  written path is read back and checked again.
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

### Agent-wide recovery metrics

Recovery counters are per agent, not per workspace. `ZedAgent::new` builds the
launch workspace runtime first and takes its
`ToolRegistry::metrics_collector()`. That same `Arc<MetricsCollector>` is
passed to `WorkspaceSetupPorts::with_metrics` and to
`WorkspaceRuntimeCache::new`, so a session workspace registry records into the
collector the agent reports from. A workspace registry built with fresh ports
would silently keep its own counters.

`ToolRegistry::set_metrics_collector` replaces the registry's collector, but it
is not a global rebind: components that cloned the previous `Arc` at
construction keep it. Paths that read the collector through
`ToolRegistry::metrics_collector()` observe the replacement; anything else does
not. Install the shared collector while building the workspace, before
dependent components capture it, rather than replacing it later.

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
