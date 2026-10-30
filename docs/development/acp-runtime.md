# ACP Runtime

This guide describes the implementation boundaries behind the
[Zed ACP integration](../guides/zed-acp.md). The ACP crate owns protocol
translation and session orchestration; reusable agent behaviour remains in
`vtcode-core`, and provider transport and custom-provider admission remain in
`vtcode-llm`.

## Boundaries and request flow

- `vtcode-acp/src/zed/` owns SACP request and notification handlers, the
  `ZedAgent` adapter, client updates, ACP tool authorization, and conversion of
  core task events into ACP messages. Keep ACP schema types and wire-specific
  conventions at this boundary.
- `vtcode-core` owns the agent loop, tool execution and task/sub-agent state.
  Its progress observer reports protocol-neutral status snapshots; it does not
  send ACP notifications.
- `vtcode-llm` owns provider interfaces and the custom-provider router. The
  router enforces the configured per-provider request limit once, so ACP and
  child-agent calls routed through the same configured provider share permits.
  ACP's provider runtime supplies cancellation, retry, deadline and telemetry
  policy without taking a second permit.

When changing a contract, keep translation at the adapter and preserve shared
core/provider behavior for non-ACP callers. The crate-specific rules are in
[`vtcode-acp/AGENTS.md`](../../crates/codegen/vtcode-acp/AGENTS.md),
[`vtcode-core/AGENTS.md`](../../crates/codegen/vtcode-core/AGENTS.md), and
[`vtcode-llm/AGENTS.md`](../../crates/codegen/vtcode-llm/AGENTS.md).

## Session workspace and durability

`session/new` requires an absolute client-selected `cwd`; ACP canonicalizes and
validates it before constructing session state. The server launch workspace is
allowed by default, with additional absolute roots configured through
`[acp.zed].allowed_workspace_roots`. Requested workspaces must fall beneath an
allowed root; filesystem roots and the home directory are rejected. The same
policy validates list, load and resume requests before workspace trust is
persisted. Each session's canonical workspace scopes its local tools, MCP
configuration, prompt context, sub-agents, compaction artifacts and archive
metadata. Do not substitute the server launch directory when the requested
workspace is invalid.

Durable session archives are controlled by `[history].persistence` and
`max_bytes`, and live under `$VT_SESSION_DIR` or the configured VT Code data
directory's `sessions/`. Checkpoints preserve user messages, assistant tool
requests, tool results, completed responses and incomplete streamed turns.
Archive metadata also carries bounded opaque ACP metadata from `session/new` and
`session/prompt`. `session/list` discovers persisted sessions and filters by
workspace when the client supplies `cwd`; `session/load` and `session/resume`
restore a session by its exact identifier, including its checkpointed
conversation. With `history.persistence = "none"`, archives and discovery are
disabled and sessions are process-local.

Compaction history under `<workspace>/.vtcode/history/` is a separate store,
controlled by `[context.dynamic]`; it supports summarization but is not a
resumable ACP archive. ACP tool audit output is a third, independent store.
Changing one store's configuration does not enable or disable the others.

## Providers

For custom-provider configuration and defaults, see
[Custom Provider Request Policy](./CUSTOM_PROVIDER_REQUEST_POLICY.md). Its
admission limit is shared by requests routed through the same configured
provider in one process, including ACP and child-agent requests. Waiting is
bounded by `queue_timeout_seconds`; cancellation applies while queued, during
retry backoff, and during an active request.

Retry is bounded exponential backoff with optional jitter. Only transient
failures before visible text or reasoning may be retried; a failure after
visible stream output is returned without replaying content and the incomplete
assistant response is checkpointed. Connection, first-token, inter-token idle,
and total-generation deadlines are configured independently. First-token and
idle limits apply to streaming calls; connection and total-generation limits
also cover buffered calls. A value of zero disables its deadline.

Provider request metrics omit provider-name labels. The shared custom-provider
admission router records current queue depth, in-flight permits, cumulative
queue wait time, and timeout count across ACP and child-agent requests, so user
configured provider names cannot create unbounded metric series.

## MCP, tools and audit

ACP initializes the session's enabled MCP providers before the first prompt, so
their direct proxy tools can be included in the initial model catalogue. The
same provider allowlists and MCP security checks used by interactive sessions
apply. The ACP primary agent's tool permissions also constrain which tools are
exposed and executable. MCP connections belong to the session that declares
them; sub-agents do not inherit them implicitly. Local tool toggles are
configured under `[acp.zed.tools]`; workspace tool policy still applies, and
client-provided filesystem capabilities and permission requests remain part of
the ACP authorization path. See the
[MCP integration guide](../guides/mcp-integration.md) and the user-facing
[ACP setup and permission guide](../guides/zed-acp.md).

The append-only JSONL audit sink is opt-in through `[acp.audit].enabled` and
`[acp.audit].path`. It records tool invocation metadata, status, timing and
hashes, not conversation text, raw arguments, or tool result bodies. It is a
security audit trail, not session history or a source for resume/compaction.

## Task updates

`vtcode-core` publishes protocol-neutral sub-agent and background-process
status snapshots through its progress observer. The ACP adapter forwards those
snapshots as extension notifications using the method `_vtcode/taskLifecycle`.
Notification parameters have `sessionId`, `acpSessionId`, and `message` fields.
`message` uses `task_started`, `task_progress`, or `task_updated` with
`task_id`, `task_type`, `subagent_type`, and the status/details for that
snapshot. Terminal updates place status and details under `patch`; non-terminal
updates place them at the message top level. A queued task or starting
background process maps to `task_started`; active work maps to `task_progress`;
completion, failure, closure, stop or error maps to `task_updated` with its
terminal status in the patch. This is a VT Code extension notification rather
than a standard ACP session update.

The model-facing `task_tracker` tool remains implemented by core. The ACP
adapter converts each successful checklist result to a standard ACP plan
update, retaining tracker metadata under `_meta.vtcode.taskTracker`. Checklist
`in_progress` and `completed` map to the corresponding ACP entry states;
blocked items are pending and marked `[blocked]`. On `session/load`,
`session/resume`, and a later prompt, ACP reads the persisted tracker state and
re-emits the plan. Replay sends a plan update only: it does not fabricate a
model-visible tool call or repeat a tool side effect.

## Crate conventions and gotchas

Module map: `capabilities/` protocol negotiation, `client/` deprecated legacy
client, `client_v2/` current client, `discovery/` agent registry, `session/`
session lifecycle, `transport/` stdio transport, `jsonrpc/` JSON-RPC types,
`tooling/` tool adapters, `zed/` Zed adapter, `workspace/` workspace helpers,
`permissions/` permission flow, `reports/` reporting, and `error/` `AcpError`.

`zed/agent/handlers.rs` is the canonical SACP handler wiring;
`zed/connection.rs` wraps `ConnectionTo<Client>`. `ZedAgent::new` attaches and
refreshes parent MCP providers before snapshotting model tools. `AcpClientV2`
is the current client API (`AcpClient` is deprecated since 0.60.x), and
`StandardAcpAdapter` / `ZedAcpAdapter` in `zed/` bridge protocol to Zed.
`register_acp_connection()` is a process-global
`OnceLock<Arc<ConnectionHandle>>`; call it once from the host protocol after
the SACP `connect_with` closure receives the `cx`. The `acp` module in
`vtcode-core` is a compatibility facade; canonical code lives in this crate.

- Update `PROTOCOL_VERSION` and `SUPPORTED_VERSIONS` together when protocol
  negotiation changes.
- `messages.rs` types are deprecated; use the `jsonrpc/` module.
- `ConnectionHandle` wraps `agent_client_protocol::ConnectionTo<Client>`.
  `cx.send_request(...).block_task()` is only safe in a `cx.spawn` task;
  calling it from a request handler deadlocks.
- The `acp` module re-exports `agent_client_protocol::schema::v1::*` plus
  `ProtocolVersion` from `schema::*`. `Client` and `Agent` (role structs) are
  at the crate root.
- `SessionUpdateNotification` decodes through a direct wire shape for SSE
  performance; new update variants must update that shape and its regression
  tests.
- Permission-flow tests use the duplex SACP connection harness; preserve
  allow, deny, cancel, unknown-option and request-failure coverage. Stdio EOF,
  timeout and cancellation paths must clear pending calls; frames and stderr
  stay bounded and sanitized.
- Provider quota observations use Lody's push-only rate-limit snapshots plus
  warning notices; never infer utilization without valid limit/remaining pairs,
  or treat `Retry-After` as a quota reset. See
  [provider rate-limit headers](provider-rate-limit-headers.md).

## Focused verification

Run the ACP and provider crate suites when changing these boundaries:

```sh
cargo nextest run -p vtcode-acp task_tracker_tool_result_emits_standard_acp_plan
cargo nextest run -p vtcode-acp forwards_lifecycle_events_through_the_official_acp_duplex
cargo nextest run -p vtcode-acp checkpoint_session_persists_messages_for_a_fresh_runtime
cargo nextest run -p vtcode-llm request_admission
```

These cover ACP plan projection and replay, duplex lifecycle forwarding,
session checkpoint persistence, and the custom-provider admission regressions.
The relevant provider retry/deadline unit tests are in the ACP
`zed/provider_runtime.rs` module; run them with
`cargo nextest run -p vtcode-acp <test-name>` after selecting a test name from
that module.
