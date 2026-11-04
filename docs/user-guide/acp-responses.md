# Use OpenAI Responses providers through ACP

Use this guide when an editor or agent client connects to VT Code through the
Agent Client Protocol (ACP) and the selected provider serves the OpenAI
Responses wire format. It explains what you see while a turn runs, how to
recover from an interrupted response, and which limits apply to tool previews.

## Configure a Responses provider

Declare the provider with `api_format = "openai-responses"`. The format applies
to ACP turns in the same way as to interactive turns:

```toml
[[custom_providers]]
name = "my-responses-gateway"
display_name = "My responses gateway"
base_url = "https://llm.example/v1"
api_key_env = "MY_RESPONSES_GATEWAY_API_KEY"
api_format = "openai-responses"
model = "my-model"
```

```text
MY_RESPONSES_GATEWAY_API_KEY=... vtcode
```

Select the provider and model in your ACP client, then start a session as
usual. The Responses format does not by itself enable WebSocket transport; the
optional WebSocket path applies only when it is configured separately.

## Completion, partial output, and recovery

A turn succeeds only after the provider sends a valid `response.completed`
event. A closed connection, truncated JSON, or an `incomplete` response is not
a successful completion.

- **Completed turn.** Assistant text, reasoning, and tool calls arrive normally.
- **Incomplete turn.** Output that was already shown stays visible, and the
  session checkpoint records the provider's stop reason. VT Code does not retry
  a generation automatically after any of its output has become visible.
- **Interrupted tool call.** Partial tool input is never permission to execute
  the tool. Execution requires the completed response.
- **Continuing the conversation.** Send a follow-up prompt. The next turn
  continues from the checkpoint instead of replaying a possibly mutating tool
  call.
- **Retries.** A retry happens before any output is visible. It may be delayed
  by `Retry-After` or a configured rate-limit window.

## Streamed tool-input previews

While a provider streams tool input, VT Code publishes a pending preview on the
tool call. The preview is advisory:

- it is labelled as **not executed** in both the pending and terminal states;
- it shows at most **8,192 characters** and appends `[preview truncated]` when
  input is longer;
- it always finishes with a terminal state before the real tool-call lifecycle
  begins;
- its identifier is distinct from the execution identifier, so a preview cannot
  be mistaken for an executed call;
- preview text is kept out of assistant messages and durable executable
  tool-call history.

When a turn fails or is cancelled, any open preview is closed as failed with a
short reason. Nothing is executed.

## Cancellation

Cancelling a turn stops the provider request and closes open previews. If the
provider had already produced output, that output remains in the session
checkpoint and the next prompt continues from it. VT Code does not replay the
cancelled generation.

## Usage errors

If the provider returns usage counters that are negative, too large for VT
Code's counters, or not numeric, the turn fails with a provider error. VT Code
does not silently treat malformed counters as zero. A missing total is derived
from input and output when both are valid.

## Optional WebSocket transport

When WebSocket transport is configured and the connection fails before any
output begins, VT Code may recover or fall back to HTTP. After output has
begun, it never replays the request over a new connection, because the provider
may already have acted on it. Reset the session or start a new prompt instead
of expecting an automatic replay.

## Related documentation

- [ACP integration reference](../acp/ACP_INTEGRATION.md)
- [ACP quick reference](../acp/ACP_QUICK_REFERENCE.md)
- [Provider rate limits](./provider-rate-limits.md)
- [Responses transport hardening](../development/responses-hardening.md)
