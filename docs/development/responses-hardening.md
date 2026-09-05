# Responses transport hardening

Custom providers with `api_format = "openai-responses"` use the Responses
wire format, including when launched through ACP. This setting does not imply
WebSocket support. The ordinary ACP path uses HTTP server-sent events (SSE);
the optional buffered WebSocket transport has a separate continuation cache.

## Completion and replay boundaries

A stream is successful only after a valid `response.completed` event.
Connection EOF, truncated JSON and `response.incomplete` are not successful
completion. Incomplete responses retain the provider's diagnostic reason.
ACP preserves partial visible output in its incomplete checkpoint and does
not retry the generation after output has become visible. A subsequent prompt
can continue from the checkpoint; it must not automatically replay a possibly
mutating tool call.

Reasoning done events contain aggregate text, not another delta. Reconciliation
preserves repeated legitimate deltas while avoiding a second copy of their
aggregate. Function and raw custom-tool input streams receive the same
incremental treatment, but incomplete tool arguments are never permission to
execute a tool. Execution still requires the completed response.

ACP publishes partial tool input as a standard pending tool preview with an
explicit “not executed” label. Preview IDs are distinct from execution IDs and
their displayed input is capped at 8,192 characters. A provider failure or
cancellation closes the preview as failed; provider completion closes it as
complete before any actual execution lifecycle begins. Preview text is not
inserted into assistant messages or durable executable tool-call history.

Events with sequence numbers are admitted only when strictly newer than the
last admitted sequence number. Duplicate and stale frames are ignored;
unsequenced compatible-provider events retain wire order. Terminal states are
absorbing. Text equality alone never establishes that an event is a duplicate.
Conflicting call/item/index aliases are errors rather than an opportunity to
combine two calls. Reasoning tracks distinguish summary/content parts within
an output item.

## Usage and request compatibility

Missing or null `usage` remains absent. A present usage object must provide
unsigned, representable input and output counts. VTCode's normalized counters
are 32-bit; malformed or overflowing counts produce a provider error rather
than silently becoming zero. An omitted total is derived by checked addition.
Optional reasoning and cached-input counts remain optional, and measured zero
is distinct from missing data. Both Responses streaming paths and buffered
parsing use the same checked conversion.

Raw custom-tool definitions omit an absent or null `format`. An explicit
format remains unchanged. Tool-call history preserves raw input bytes and
function JSON strings, and pairs outputs with their original call IDs.

## WebSocket cancellation

An in-flight exchange owns its socket. Cancelling its future drops the socket
and invalidates continuation, so the next turn cannot consume abandoned
response events. Only a completed exchange returns a socket to the cache.
After output has begun, a WebSocket failure cannot reconnect or silently fall
back to HTTP. Pre-output recovery remains available for supported errors.
Continuation is provider-instance-local and requires compatible model,
instructions, tools and history.

## Verification boundaries

The tests distinguish the legacy `provider.stream` path used by ACP from the
normalized processor used by the core runtime. Passing a generic
OpenResponses compliance test is not evidence that either wire decoder works.

Unit regressions cover reasoning reconciliation, missing completion, incomplete
diagnostics, optional tool format and checked usage. Property tests exercise
network-byte partitions, lifecycle/call histories, numeric boundaries and
tool-call/history round trips. Scripted WebSocket tests exercise recovery,
cancellation during a pending read and rejection of replay after output.

Responses-specific ACP tests use the official Rust ACP client, the real custom
provider router and the `/responses` endpoint. They observe session updates,
retry notices, partial checkpoints and continuation rather than testing only
an HTTP decoder. VidaiMock physics fixtures use Responses start/chunk/stop
templates, with a gateway only for faults or headers the pinned mock cannot
express. Deterministic clock tests and physical pacing are complementary:
neither substitutes for the other.

Bounded Kani checks exercise a deterministic reducer used by production code,
not a second implementation written solely for proofs. The proof bounds and
unwind checks are part of its contract; they do not prove arbitrary Tokio
scheduling, network behaviour or unbounded session histories. Use
[rust-prover-tools](https://github.com/leynos/rust-prover-tools) to check the
pinned verifier version before running the narrow proof harness. Mutation
witnesses must demonstrate that the new properties and proofs reject broken
production behaviour.

At the restart checkpoint, ordinary unit, property and ACP behavioural tests
and the VidaiMock 0.3.1 Responses physics tests have passed. The revised Kani
base/inductive-step and snapshot proofs, deliberate mutation witnesses and
live Friendli compatibility checks remain pending. The presence of these
harnesses is not a claim that all formal or live-provider validation is complete.

## Live compatible-provider checks

Live checks are opt-in and use synthetic prompts with explicit token budgets.
Never record authorization headers, key-file contents or workspace documents
in replay fixtures. Record the model, endpoint, event vocabulary, terminal
status and reported usage, and distinguish actual measurements from pricing
estimates. Keep a working Chat profile alongside an experimental Responses
profile until model-specific compatibility has been demonstrated.

The ignored `friendli_responses_bounded_compatibility` integration test requires
both `VTCODE_FRIENDLI_LIVE=1` and `VTCODE_FRIENDLI_LIVE_KEY_FILE`. It makes at most
four requests capped at 2,048 output tokens each (8,192 total), has no automatic
retry loop and runs no returned tools. Its synthetic cases cover buffered text,
streamed text, a function call and a raw custom call. Do not enable it in ordinary
CI or run it repeatedly without reassessing the live budget.

Friendli documents [Responses with SSE](https://friendli.ai/docs/openapi/model-apis/responses)
as beta. Its published schema does not establish WebSocket support; do not
enable WebSockets merely because its HTTP endpoint is OpenAI-compatible.
