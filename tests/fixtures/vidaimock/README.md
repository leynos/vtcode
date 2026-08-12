# VidaiMock Arli scenarios

These fixtures drive the Arli ACP probe with VidaiMock 0.1.3. The baseline
stream in [`arli_chat_stream.json`](arli_chat_stream.json) and
[`templates/arli/captured_stream.j2`](templates/arli/captured_stream.j2) comes
from a sanitised mitmproxy reverse-proxy capture. It retains response timing
and SSE frames; request headers and the request body were removed.

## Run the probe

Use the official ACP Rust SDK as the client harness. Start the mock from the
fixture repository with the scenario you want and the provider/template
directory in this directory:

```bash
cd /home/leynos/Projects/vtcode-arli-toolcall-fixture
VIDAIMOCK_ISOLATED=true vidaimock \
  --config /home/leynos/Projects/VTCode/tests/fixtures/vidaimock/scenarios/success.toml \
  --config-dir /home/leynos/Projects/VTCode/tests/fixtures/vidaimock
```

The provider route is defined in [`providers/arli-replay.yaml`](providers/arli-replay.yaml).
Run the ACP Rust SDK harness against the mock's local endpoint, then repeat
with another file in `scenarios/`.

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

VidaiMock 0.1.3 interprets the chaos percentages as values from `0.0` to
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
