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
`SessionToolCatalogueSnapshot::tool_catalogue_hash`,
`AlignmentError::PromptCatalogueMetadataMismatch`,
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
- `ThreadCompactBoundaryEvent` has the Rust fields `previous_catalogue_hash`
  and `new_catalogue_hash`. The serde wire keys stay `previous_catalog_hash` and
  `new_catalog_hash`, pinned by `#[serde(rename = "...")]`, so the event
  schema is unchanged.
- The pod store keeps reading and writing `catalog.json`. Only the accessor
  name changes, so existing deployment profiles continue to load.
- Runtime prompt metadata uses the `[Runtime Tool Catalogue]` header.
  Refreshing a prompt removes a legacy `[Runtime Tool Catalog]` section while
  preserving notices appended after it, and `stable_system_prefix_hash` ignores
  metadata under either header.

Do not add aliases for the old Rust names, and do not rewrite persisted data to
match the new spelling.

The skill-installer sample follows the same rule inside its helper script:
`fetch_catalogue` and `EMBEDDED_CATALOGUE` replace the American spellings.
Those names are internals of
`crates/codegen/diapason-skills/src/skills/assets/samples/skill-installer/scripts/list-curated-skills.py`,
not a published reuse API, so no external caller depends on them.

## Zed extension API

The Zed extension uses the native `analyse` spelling in its Rust API. The old
names are removed rather than deprecated:

- `CommandBuilder::analyze()` is now `CommandBuilder::analyse()`, which builds
  the `analyse` command.
- `analyze_workspace` and `analyze_workspace_with_context` are now
  `analyse_workspace` and `analyse_workspace_with_context`.
- `analyze_workspace_command` is now `analyse_workspace_command`.
- `WorkspaceConfig::analyze_on_startup` is now
  `WorkspaceConfig::analyse_on_startup`.

`WorkspaceConfig` keeps the serialized TOML key `analyze_on_startup` through
`#[serde(rename = "analyze_on_startup")]`, so existing `config.toml` files need
no edit. `execute_command` still accepts the legacy `analyze` command and
selects the same 60-second workspace-analysis duration for both spellings.
Command execution does not yet enforce a deadline, a limitation that predates
this layer. See the
[Zed extension quick start](../extensions/zed-extension/docs/QUICK_START.md).

## Analyse command skill compatibility

The bundled analysis command skill has the canonical ID `cmd-analyse` and the
bundled path `.system/cmd-analyse`. In
`crates/codegen/diapason-skills/src/command_skills.rs`,
`LEGACY_ANALYSE_COMMAND_SKILL_ID` holds the legacy ID `cmd-analyze`, and
`command_skill_id_alias` maps either spelling to the other.
`find_command_skill_by_skill_name` accepts both IDs, so `/skills info` and
`/skills use` resolve the same skill from either spelling, and the `analyze`
slash alias continues to route to it.

Alias precedence is one rule, expressed by `SkillScope`: `Repo` outranks
`User`, which outranks `Admin`, which outranks the bundled `System` scope.
Within one scope the first discovered root wins, so the pair is never resolved
by sorted order. Direct resolution by `skill_metadata_by_name` and the
persisted preload lookup (`loop_skills` via `preloaded_skill_metadata_by_name`)
in `crates/codegen/diapason-core/src/skills/manager.rs` share that rule.
Unrelated exact-name duplicates keep their separate contract: the preload
lookup keeps the last entry by design, not as a second precedence rule.

Command skills never enter the model-invocable catalogue. Both IDs are excluded
from the `## Skills` prompt section, so use slash completion or `/skills`
discovery to reach them.

## Workflow validation tooling

`make lint` runs yamllint with the repository `.yamllint.yml` policy and then
actionlint through `make github-actions-lint`, so malformed YAML fails before
the GitHub Actions-specific checks. The full configuration details are in
[docs/development/ci-cd.md](./development/ci-cd.md).

- `make test-github-actions-validation` runs the controlled-linter contract
  tests through `uv` with PyYAML 6.0.3.
- CI pins yamllint 1.38.0 and actionlint 1.7.12, and verifies the actionlint
  archive checksum before running the reviewed installer.

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

### Controller locks and background ledger

`SubagentLockScope` owns the two locks a controller tree shares: an adoption
mutex that serializes subtree grafting and child-id allocation, and a
persistence mutex that serializes background-ledger saves.
`SubagentControllerConfig.lock_scope: Option<Arc<SubagentLockScope>>` selects
the scope. `None` resolves to `default_lock_scope()`, one process-global
instance, so controllers built without a scope still exclude each other across
the process. A nested child controller starts with `None` and then calls
`inherit_lock_scope` to join its parent's scope. An injected scope is
deliberately isolated: grafting a controller bound to a different scope is
rejected rather than merged into one tree.

A ledger save also takes an advisory inter-process lock on
`.diapason/state/background_subagents.lock` (under the configured workspace
directory) and holds it across the reload, merge and write. Acquisition retries
a non-blocking `flock` with async backoff from 5 ms, doubling to 250 ms, under
a 10 s deadline. On expiry the save fails with a contextual error and writes
nothing; see `subagents/background_lock.rs`.

Background record ids are owner-scoped. `background_record_id_for_owner` appends
`@<owner>` to `background-<agent>` and percent-encodes every owner byte outside
`[A-Za-z0-9_-]` as uppercase `%XX`, so distinct owners cannot collide. An
absent or empty owner keeps the unscoped id. A save merges rather than
replaces: records join on their canonical id, the copy with the newest
`updated_at` wins, a record only one side holds is kept, and an owner-scoped
save removes the records its session no longer tracks. A persisted cancellation
of an exec session outranks a later refresh of the same launch, so a stale copy
cannot resurrect a stopped task.
