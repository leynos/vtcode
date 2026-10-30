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

Compatible endpoints may emit `response.reasoning_part.added` and
`response.reasoning_part.done` with a nested `reasoning_text` part. VTCode
reconciles its text as a snapshot, preserving an initial nonempty prefix and
avoiding duplicate done text. Missing or malformed parts remain errors; this
does not make unknown event names silently acceptable. The fixture shape follows
the [vLLM Responses protocol](https://docs.vllm.ai/en/v0.23.0/api/vllm/entrypoints/openai/responses/protocol/).

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

## Compatible-provider function-call IDs

Responses providers should keep a function call's streamed `call_id` in the
terminal `response.completed` item. VT Code rejects a mismatched ID by default.
This prevents a terminal call from being correlated with the wrong streamed
arguments and ensures that no call becomes executable before final
reconciliation succeeds.

For a custom provider known to rewrite function-call IDs, set
`responses_allow_function_call_id_remap = true` on the provider or its exact
model profile. The compatibility mode accepts only a one-to-one match between
unmatched ordinary function calls with the same non-empty name and equal,
complete strict JSON arguments. It rejects duplicate IDs, duplicate semantic
payloads, partial JSON, missing calls, contradictory calls and custom/freeform
calls. Exact IDs are matched first, and accepted calls retain the terminal ID.

Keep the capability absent or `false` for providers that preserve IDs. An
explicit model-profile `false` overrides a provider-level `true`.

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

Formal checks target the production decision functions, not the entire heap
representation. Lifecycle admission covers six events. Identity lookup uses
two fixed registered calls and symbolic query classes: exact or partial
matches, unknown identities, conflicting aliases and missing keys. A separate
base-case check exercises production capture of two calls. Snapshot checks
exercise the actual reconciliation helper over representative prefix relations;
reasoning identity checks cover output/part-index separation.

Payload append, arbitrary alias enrichment, registration histories and the
allocating `custom_tool_calls()` projection remain unit/property-test
obligations, not formal claims. Proof-local values may be forgotten after all
assertions: allocator teardown is outside this state contract. No allocator
growth-policy assumption or modified pointer encoding is required.

All six bounded harnesses passed with Kani 0.67.0 and safety/unwind checks
enabled. Routing uses an unwind bound of 64 to include bytewise comparison
of its full diagnostic strings; the bound is not a claim of 64-call coverage.

All 13 new properties and all six proofs rejected deliberate production
mutations with meaningful assertion failures, not compilation failures or
solver exhaustion. The following mutation groups provide reproducible
witnesses; restore production code before running ordinary gates:

| Production fault | Property/proof obligation exercised |
| --- | --- |
| Ignore changed continuation model | WebSocket context isolation property |
| Serialize a custom output as a function output | Tool-history round-trip property |
| Cast oversized counters or wrap their sum | Both usage properties |
| Prepend decoded SSE frames | Both byte-partition/framing properties |
| Admit an equal sequence or post-terminal event | Sequence and terminal properties; lifecycle proof |
| Route a matched call to slot zero | Interleaved payload property |
| Bypass conflicting item aliases | Alias-conflict property; routing proof |
| Append a complete snapshot again or accept divergence | Three reasoning properties; snapshot proof |
| Ignore reasoning output index | Reasoning-index proof |
| Replace a captured call name | Two-call capture proof |
| Prefer stale final input over longer streamed input | Final-input preference proof |

Ordinary unit, property and ACP behavioural tests and VidaiMock 0.3.1 physics
tests passed at the restart checkpoint. Live Friendli compatibility is checked
separately; offline verification alone does not establish provider support.

## Live compatible-provider checks

Live checks are opt-in and use synthetic prompts with explicit token budgets.
Never record authorization headers, key-file contents or workspace documents
in replay fixtures. Record the model, endpoint, event vocabulary, terminal
status and reported usage, and distinguish actual measurements from pricing
estimates. Keep a working Chat profile alongside an experimental Responses
profile until model-specific compatibility has been demonstrated.

The ignored `friendli_responses_selected_compatibility_probes` integration test
requires explicit live, key-file, case-selection and private capture-directory
settings. Each selected synthetic case makes one request capped at 2,048 output
tokens, redirects and automatic retries are disabled, and the total requested
budget may not exceed 10,000 tokens. Returned tools are never executed. Cases
are independent: one failure does not prevent capture of the remaining selected
cases. See [Friendli Responses compatibility probes](friendli-responses-probes.md)
for the operator contract and exact outcome rules.

The sanitized 2026-09-05 captures show one function call whose streamed and
terminal IDs differ, two required-custom requests that returned HTTP 500, and
two named-custom requests that returned HTTP 200 with prose but no custom call.
The latter are `no_call` compatibility failures, not successes. These captures
motivate offline remapping and ACP replay coverage, but no later paid run has
established current function- or custom-tool compatibility through VT Code.

Friendli documents [Responses with SSE](https://friendli.ai/docs/openapi/model-apis/responses)
as beta. Its published schema does not establish WebSocket support; do not
enable WebSockets merely because its HTTP endpoint is OpenAI-compatible.
