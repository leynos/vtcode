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
  ACP's provider runtime supplies cancellation, retry, deadline, and telemetry
  policy without taking a second permit.

When changing a contract, keep translation at the adapter and preserve shared
core/provider behaviour for non-ACP callers. The crate-specific rules are in
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

## Interrupted tool recovery

A turn can end after the model emits tool calls but before every call has a
terminal result, for example when the process exits or an archive is resumed
mid-round. ACP closes that gap with a write-ahead protocol instead of replaying
mutating tools:

- Before executing a tool round, ACP appends an incomplete result placeholder
  for every call and requires a durable checkpoint. If that checkpoint fails,
  the prompt fails and no tool runs.
- After execution, ACP replaces the placeholders with terminal results and
  checkpoints again, so a resumed archive never observes an unresolved batch.
- `session/load` and `session/resume` repair legacy archives by inserting an
  explicit incomplete result before later messages, then checkpoint the
  repaired history before the session becomes visible. The repair records that
  the call was not replayed and tells the model to verify workspace state
  before resubmitting.
- A repair that only relocates a uniquely attributable late result changes
  message order, so it is persisted and checkpointed exactly like a repair that
  inserts a placeholder.
- History with duplicate or otherwise ambiguous terminal results is rejected
  rather than guessed at. The archive is left untouched and the session does
  not become resumable. A compacted history whose retained prefix begins with
  Tool results is not ambiguous: only front trimming can produce that shape,
  and providers strip orphan tool blocks at serialization, so it resumes. Every
  duplicate, mismatched, or orphan result inside the retained history is still
  rejected.
- `write_stdin` reports partial state when the session is lost after the input
  write, because the bytes may already have been consumed. A pre-write
  resolution failure and a poll or wait failure do not.

Recovery never executes a tool a second time. Related failures carry typed
outcomes: a `write_stdin` continuation for a terminated exec session becomes a
non-retryable resource-not-found error with no partial-state claim, and a
checkpoint failure is logged with its operation and error category before the
prompt is rejected. Prompt admission remains single-turn, so a concurrent
prompt is rejected without mutating history.

Focused verification:

```sh
cargo nextest run -p vtcode-acp attaching_legacy_archive_repairs_without_replaying_mutating_call
cargo nextest run -p vtcode-core stale_write_stdin_modes_are_resource_not_found_without_partial_state
```

### Recovery observability

`metrics::tool_recovery_metrics` counts recovery events in bounded,
fixed-variant counters: staged calls, replaced calls, interrupted calls,
relocated results, repaired archives, ambiguous histories, concurrent prompt
rejections, stale `write_stdin` requests, and checkpoint failures. The snapshot
appears as `tool_recovery` in the metrics summary JSON and as
`vtcode_tool_recovery_total{event=...}` in the Prometheus export.

Every label is a fixed event variant. Session identifiers, tool-call
identifiers, tool names, and tool output never become a label or a log value,
so a session can export the snapshot without leaking which tools it invoked or
what they returned. Adding a dimension means adding an enum variant and its
`as_str` arm; a free-form string label is not acceptable.

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

### Provider rate limits and retry floors

Provider quota headers become typed `RateLimitMetadata` in `vtcode-llm` rather
than free-form diagnostics. Custom providers map response header names under
`[custom_providers.rate_limit_headers]`, and known provider names supply their
provider-specific defaults. Mapping precedence, provider vocabulary, and
malformed-value handling are in the
[provider rate-limit header reference](./provider-rate-limit-headers.md); the
user-facing configuration is in the
[rate-limit guide](../user-guide/provider-rate-limits.md).

`Retry-After` and a configured reset interval produce a retry floor: the
decision delay is the larger of the ordinary exponential backoff and the
provider minimum. `RetryBackoff` owns that floor for one logical retry
generation. It retains the largest provider minimum seen so far across later
failures that omit the header, and resets at the start of the next generation,
including the next tool-loop segment. Jitter can lengthen a wait but never
shorten the floor, and the local maximum backoff cannot undercut it.

Absolute `Retry-After` HTTP dates are meaningful only relative to the instant
the failure was observed, so retry decisions take an explicit observation time.
`RetryPolicyCoreExt::decision_for_llm_error_at` and
`decision_for_llm_error_with_backoff_at` accept a `SystemTime` or
`LlmRetryAttempt`, and `VtCodeError::from_llm_error_at` converts with the same
instant. The clock-free compatibility adapters (`decision_for_llm_error`,
`decision_for_anyhow`, and `From<LLMError> for VtCodeError`) honour numeric
deltas and configured reset intervals but ignore absolute dates: reading the
wall clock inside a decision would classify the same error differently across
calls. New code at a boundary that knows the observation time must use the
explicit-time method.

ACP projects the same metadata twice. The warning notice carries the retry
instruction and configured quota values in `_meta.lody.notice`; the optional
push-only `_lody/rate_limits/update` notification carries a snapshot with a
stable `limitId`, provider/model scope, and bounded utilisation. Neither path
infers an account identity, a quota query API, or a reset time from
`Retry-After`. Failures to deliver either notification are counted by the
bounded `vtcode.acp.rate_limit_notification_failure_total` metric, tagged by
provider class and notification operation, and never logged with a provider
name or response body.

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

## Diagnostics privacy

The repaired ACP failure boundaries — the `list_files` tool boundary and
optional-component setup — publish a fixed message and bounded fields:
`operation`, `component`, `outcome`, `error_category`, and, for suppressed
failures, `error_chain_depth`. Their client-facing failures are short fixed
messages that do not embed workspace paths, command text, environment values,
or tool payloads. That guarantee is scoped to those boundaries; it is not a
repository-wide claim that every ACP client response is detail-free. Any
surface that turns a failure into a client error — REST error bodies as well as
logs — needs the same treatment; sanitizing only the log line still leaks.

Provider incomplete-turn notices are outside that scope. They deliberately
include the provider error text so the caller can see what failed, and the
notice strips ANSI escape codes as presentation hygiene rather than privacy
redaction.

At the repaired boundaries the originating `anyhow` chain is retained at
`debug` for local diagnosis. That is a diagnostic aid, not redaction: debug
logs can contain workspace paths, provider detail, and prompt or tool payloads.
Treat them as sensitive and collect or share them deliberately. Severity
suppression alone is not redaction; where detail must never surface, remove it
from the value that carries it rather than lowering the level it is logged at.

## Property-model limits

The ACP lease, session-activity, and cancellation models are sequential
reference models. They check that each operation's bookkeeping matches an
independent model, and they drive the production acquire, release, drop, close,
and notification paths rather than assigning lifecycle state directly. They say
nothing about concurrent interleavings: passing them is not evidence that
simultaneous acquisition, release, and shutdown are safe.

Notification regressions need a waiter that is registered before the final
departure. A waiter created afterwards completes from the counter alone and
therefore does not detect a missing final-guard notification.

## Subagent controller and task updates

`vtcode-core` publishes protocol-neutral sub-agent and background-process
status snapshots through `SubagentProgressObserver`. The ACP adapter owns all
protocol projection.

### Session-scoped controller resolution

A session resolves its own `SubagentController` from its `SessionHandle`. If
none is available, management requests return
`subagent_management_unavailable`. Management operations walk the root
controller and every reachable descendant controller, so a session can list,
cancel, or read output for any task it owns, including grandchildren. A session
lists and manipulates only tasks it owns: child agents are selected through
their `parent_thread_id` chain, while managed background records are selected
by their explicit `owner_session_id`. A task id outside that owned set and an
id that does not exist both return the same `unknown_task` error, so the
boundary does not disclose another session's tasks.

### Lifecycle projection

Each snapshot is projected into a standard ACP `session/update`. The first
update for a task is a tool call with id `task:<taskId>`; later updates are
tool-call updates for that same id. The standard `title`, `kind` (`think`),
`status`, and `content` fields carry the task state, and `_meta.lody.task`
carries the Lody snapshot: `version`, `taskId`, `kind`, `status`, `description`,
`actor`, start and end times, and optional `summary` or `error`. Queued
sub-agents and starting background processes report `pending`; running work
reports `in_progress`; completion, failure, closure, stop, or error reports the
corresponding terminal status. The adapter filters updates by parent session,
drops superseded revisions by `updated_at`, and replays still-active tasks when
the observer is registered.

### Management RPCs

When management is advertised, `_lody/subagents/list`,
`_lody/subagents/cancel`, and `_lody/subagents/output` accept a `sessionId`
plus their operation fields. `list` accepts `activeOnly` and refreshes
background records from the exec-session manager before projecting, so an
exited process is not reported as running. `cancel` closes a child agent or
force-cancels a background record. `output` returns a bounded tail: `tail`
defaults to 200 lines, and values above the configured maximum are rejected with
`invalid_tail`. Each request re-resolves the session's controller tree.

### Usage publication

After each provider response, ACP sends `_lody/session/usage_update` with
`sessionId`, a per-response `usage` delta (`inputTokens`, `outputTokens`,
`cacheReadInputTokens`, and optional `cacheCreationInputTokens`), and
`modelUsage` keyed by the responding model. The notification is omitted when
the response carries no usage. The delta covers that response, not a running
session total.

Custom providers opt in to streamed usage with `supports_stream_usage` in their
provider defaults or model profile. The OpenAI-compatible provider requests
usage from the stream only for native OpenAI or when the resolved profile sets
it, so a provider that does not declare support is never asked.

### Task tracker plans

The model-facing `task_tracker` tool remains implemented by core. The ACP
adapter converts each successful checklist result to a standard ACP plan
update, retaining tracker metadata under `_meta.vtcode.taskTracker`. Checklist
`in_progress` and `completed` map to the corresponding ACP entry states;
blocked items are pending and marked `[blocked]`. On `session/load`,
`session/resume`, and a later prompt, ACP reads the persisted tracker state and
re-emits the plan. Replay sends a plan update only: it does not fabricate a
model-visible tool call or repeat a tool side effect.

## Compaction activity and failure reporting

Automatic compaction publishes a paired activity over standard ACP session
updates. `lody_activity::CompactionActivity` projects a started `tool_call` and
a terminal `tool_call_update`, both carrying `_meta.lody.activity` with
`kind = "context_compaction"`. The terminal update sets `status = failed` when
a failure category is present; otherwise it reports `completed`, with
`compacted` distinguishing a reduction from a no-op. Failures carry a
`failureReason` drawn from a closed set: `cancelled`, `timeout`,
`admission_budget`, `pre_compact_hook`, or `provider`.

`vtcode-exec-events::ThreadEvent` remains the authoritative runtime event
contract, including its compaction boundary events. The ACP activity is a
client projection of that runtime outcome, not a parallel event type: keep new
compaction facts in the core event contract and translate them at the adapter.

`compaction::failure::CompactionFailure` keeps the closed-set category separate
from any untrusted provider or hook detail. Its `Display` renders only a fixed,
client-safe message, so `failureReason` cannot leak a provider payload,
workspace path, or archive name. The retained detail is exposed through the
error source chain and is rendered only by the redacted detail helper, which
collapses path-like tokens, masks recognised secrets, and bounds the result to
512 bytes; `log_suppressed_error` emits that rendering at `debug` level.

Bounded, process-local counters accumulate across compaction attempts:
`started`, `compacted`, `no_change`, `failed`, `cancelled`, `elapsed_ms_total`,
and `elapsed_ms_max`. Read them with
`vtcode_acp::metrics::compaction_metrics()`; the snapshot is cumulative and
never reset, and `cancelled` is a subset of `failed`. Cardinality is fixed by
construction: no session, provider, model, or path value is recorded. Spans and
logs correlate attempts with `session_correlation`, a deterministic, truncated
SHA-256 digest, never the raw session identifier.

## Retry notices and usage publication

Provider retry orchestration reports attempt failures through the narrow
`AttemptFailureSink` port rather than through ACP types. The buffered
generation path constructs a `RateLimitNoticeSink`, the ACP adapter that
implements the port, and passes it into `generate_with_retry`; the streaming
path calls the same `publish_rate_limit_notice` helper directly. The adapter
turns a classified retryable or terminal rate limit into a session-info notice
carrying `_meta.lody.notice`, reporting only the status, `Retry-After` value,
and scheduled delay the provider actually supplied. It is a presentation
adapter, not the JSON-RPC transport: it neither retries the request nor changes
retry policy.

`publish_lody_usage` emits the `_lody/session/usage_update` extension
notification for each provider response. It resolves the effective model first:
the model named by the response wins, and the session model is the fallback.
That single model then selects the usage profile: for a configured custom
provider, `resolved_profile` layers the exact model profile over the provider
defaults for context window, pricing, and API format; otherwise the built-in
model catalogue supplies what it knows. Reasoning tokens are reported in
`reasoningOutputTokens` and subtracted from `outputTokens`, so the two fields
describe disjoint token sets. Cost is omitted when the profile supplies no
pricing. For the user-facing contract, see
[ACP usage, compaction, and subagents](../users-guide.md#acp-usage-compaction-and-subagents);
the wire shape is in the [ACP integration guide](../acp/ACP_INTEGRATION.md).

## Focused verification

Run the ACP and provider crate suites when changing these boundaries:

```sh
cargo nextest run -p vtcode-acp task_tracker_tool_result_emits_standard_acp_plan
cargo nextest run -p vtcode-acp forwards_lifecycle_events_through_the_official_acp_duplex
cargo nextest run -p vtcode-acp checkpoint_session_persists_messages_for_a_fresh_runtime
cargo nextest run -p vtcode-acp -E 'test(session_activity_matches_the_reference_model)'
cargo nextest run -p vtcode-acp -E 'test(workspace_lease_accounting_matches_the_reference_model)'
cargo nextest run -p vtcode-llm request_admission
```

These cover ACP plan projection and replay, duplex lifecycle forwarding,
session checkpoint persistence, the sequential activity and lease reference
models, and the custom-provider admission regressions. The relevant provider
retry/deadline unit tests are in the ACP `zed/provider_runtime.rs` module; run
them with `cargo nextest run -p vtcode-acp <test-name>` after selecting a test
name from that module.
