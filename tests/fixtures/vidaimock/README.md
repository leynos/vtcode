# VidaiMock Chat and Responses scenarios

The Chat Completions fixtures drive the Arli ACP probe with VidaiMock 0.3.1.
The baseline stream in [`arli_chat_stream.json`](arli_chat_stream.json) and
[`templates/arli/captured_stream.j2`](templates/arli/captured_stream.j2) comes
from a sanitised mitmproxy reverse-proxy capture. It retains response timing
and SSE frames; request headers and the request body were removed.

## Run the probe

Use the official ACP Rust SDK as the client harness. Start the mock from the
fixture repository with the scenario you want and the provider/template
directory in this directory:

```bash
cd /home/leynos/Projects/vtcode-arli-toolcall-fixture
VT_FIXTURES=/home/leynos/Projects/VTCode/tests/fixtures/vidaimock
VIDAIMOCK_ISOLATED=true vidaimock \
  --config "$VT_FIXTURES/scenarios/success.toml" \
  --config-dir "$VT_FIXTURES"
```

The captured replay route is defined in
[`providers/arli-replay.yaml`](providers/arli-replay.yaml). The separate
[`providers/arli-physics.yaml`](providers/arli-physics.yaml) route generates
four distinct content chunks at `/v1/physics/chat/completions`; it exists so
VidaiMock applies latency, trickle, and disconnect behaviour to actual model
frames rather than to an already-rendered capture.

Issue 21 child-planner recovery fixtures keep two contracts explicit:

- [`planner-response-sanitized.json`](planner-response-sanitized.json) is a
  sanitised, structurally valid success response derived from the supplied
  capture. It is not the historical failing response.
- [`planner-response-empty.json`](planner-response-empty.json), the malformed
  scenario, and the provider-drop scenario deliberately exercise empty
  content, invalid transport JSON, and provider failure.
- [`providers/child-single-physics.yaml`](providers/child-single-physics.yaml)
  serves one minimal buffered child response at
  `/v1/child-single/chat/completions`. Its request count can prove that a
  delegated child does not issue an optional planner request before executing
  its single task.

The [`providers/responses-physics.yaml`](providers/responses-physics.yaml)
route uses VidaiMock 0.3.1 to apply the same deterministic scenarios to OpenAI
Responses SSE frames at `/v1/responses-physics/responses`. VidaiMock extracts
the stream source from the structured Responses `output` array, while the raw
lifecycle templates emit typed output-text events followed by reasoning and
terminal usage. This lets ACP tests distinguish the Responses wire path from
Chat Completions without a Chat-shaped tokenizer shim.

The
[`providers/responses-physics-truncated.yaml`](providers/responses-physics-truncated.yaml)
route deliberately omits the stop lifecycle at
`/v1/responses-physics-truncated/responses`. It produces real paced Responses
deltas and then a clean EOF without `response.completed`, giving the ACP tests
a deterministic visible-prefix truncation. VidaiMock's percentage disconnect
fault is evaluated before every frame, so a 100% setting may disconnect before
the first visible delta and is not suitable for this assertion.

Run the ignored adapter physics tests explicitly:

```bash
cargo nextest run --run-ignored only \
  -p vtcode-llm vidaimock_

cargo nextest run --run-ignored only \
  -p vtcode-acp vidaimock_
```

The `vtcode-llm` Chat and `vtcode-acp` Responses tests require VidaiMock 0.3.1.
Each test checks that version on `PATH`, binds the mock to an ephemeral
localhost port, and terminates the child process after the scenario.

All scenario files use VidaiMock's exact configuration sections and keys:

```toml
[latency]
mode = "realistic"
base_ms = 0
jitter_pct = 0.0

[chaos]
enabled = false
drop_pct = 0.0
malformed_pct = 0.0
trickle_ms = 0
disconnect_pct = 0.0
```

VidaiMock 0.3.1 interprets the chaos percentages as values from `0.0` to
`100.0`; deterministic scenarios therefore use `100.0`. The success scenario
uses the captured 2,189 ms TTFT. The delayed-first-token and
trickled-stream scenarios isolate first-token latency and inter-frame pacing;
the remaining scenarios exercise parser and recovery paths.

Connection refusal is tested by pointing the ACP client at an unopened port;
it is not a VidaiMock scenario. To test repeated provider `500` responses and
recovery, keep the ACP connection alive, restart VidaiMock on the same port
with `provider-drop-500.toml`, then restart it again with `success.toml` and
send the next prompt.

| Scenario | File | Deterministic behaviour |
| --- | --- | --- |
| Success | [`success.toml`](scenarios/success.toml) | Captured 2,189 ms TTFT. |
| Delayed first token | [`delayed-first-token.toml`](scenarios/delayed-first-token.toml) | Adds a fixed first-token delay. |
| Trickled stream | [`trickled-stream.toml`](scenarios/trickled-stream.toml) | Spaces stream chunks by a fixed interval. |
| Mid-stream disconnect | [`mid-stream-disconnect.toml`](scenarios/mid-stream-disconnect.toml) | Disconnects every streaming response. |
| Provider drop / `500` | [`provider-drop-500.toml`](scenarios/provider-drop-500.toml) | Returns a `500` for every request. |
| Malformed response | [`malformed-response.toml`](scenarios/malformed-response.toml) | Returns malformed JSON for every request. |
