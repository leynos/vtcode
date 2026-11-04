# Open Responses Specification Conformance

<a href="https://www.openresponses.org/"><img
src="https://img.shields.io/badge/Open%20Responses-Conformant-4CAF50?style=flat-square"
alt="Open Responses Conformant"/></a>

Diapason conforms to the [Open Responses](https://www.openresponses.org/)
specification, an open, vendor-neutral standard for large language model APIs.
This enables interoperable LLM workflows across different providers.

## Conformance Overview

Diapason provides full conformance with the Open Responses specification in two
ways:

1. **Producer Conformance**: Diapason can emit Open Responses-conformant events
    and objects from its internal agent loop, allowing external tools to
    monitor and interact with Diapason using a standardized protocol.
2. **Consumer Conformance**: Diapason can use any Open Responses-compatible API
    as a backend provider, enabling seamless switching between different LLM
    providers that support the standard.

## What is Open Responses?

Open Responses is an open-source specification for building **multi-provider,
interoperable LLM interfaces** based on the OpenAI Responses API. It defines:

- **Shared Schema**: Unified request/response structures across providers
- **Semantic Streaming**: Events describe meaningful transitions, not raw token
  deltas
- **Agentic Loop Support**: Composable tool invocation and message orchestration
- **Extension Points**: Provider-specific features via namespaced extensions

## Implementation Details

The Open Responses implementation is located in
`crates/codegen/diapason-core/src/open_responses/` (for production) and
`crates/codegen/diapason-core/src/llm/providers/openresponses/` (for
consumption). It provides:

- **Unified Item Types**: State machine-based items with defined lifecycle
  states
- **Semantic Streaming Events**: Meaningful events (not raw token deltas) for
  predictable streaming
- **Response Objects**: Standardized structure per the specification
- **Error Handling**: Structured errors with type, code, param, and message
  fields
- **Extension Points**: Support for Diapason-specific item types and events

## Conformance Levels

Diapason implements the following core components of the specification:

### 1. Items as State Machines

All items follow a state machine model with defined lifecycle states:

```
┌─────────────┐
│ in_progress │──────────────────────────────────┐
└──────┬──────┘                                  │
       │                                         │
       ▼                                         ▼
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ incomplete  │     │  completed  │     │   failed    │
└─────────────┘     └─────────────┘     └─────────────┘
  (terminal)          (terminal)          (terminal)
```

```rust
use diapason_core::ItemStatus;

let status = ItemStatus::InProgress;
assert!(!status.is_terminal());

let completed = ItemStatus::Completed;
assert!(completed.is_terminal());
assert!(completed.is_success());
```

### Semantic Streaming Events

Streaming is modeled as semantic events, not raw text deltas:

| Event Type                   | Description                    |
| ---------------------------- | ------------------------------ |
| `response.created`           | Initial response creation      |
| `response.in_progress`       | Response processing started    |
| `response.output_item.added` | New output item added          |
| `response.output_text.delta` | Text content delta             |
| `response.output_text.done`  | Text content complete          |
| `response.output_item.done`  | Output item complete           |
| `response.completed`         | Response finished successfully |
| `response.failed`            | Response failed with error     |

```rust
use diapason_core::{ResponseStreamEvent, Response, VecStreamEmitter, StreamEventEmitter};

let mut emitter = VecStreamEmitter::new();
let response = Response::new("resp_123", "gpt-5");

emitter.response_created(response.clone());
emitter.output_text_delta("resp_123", "item_0", 0, 0, "Hello, ");
emitter.output_text_delta("resp_123", "item_0", 0, 0, "world!");

let events = emitter.into_events();
assert_eq!(events.len(), 3);
```

### Output Item Types

Diapason supports these Open Responses item types:

| Type                   | Description                      |
| ---------------------- | -------------------------------- |
| `message`              | User/assistant/system messages   |
| `reasoning`            | Model's internal thought process |
| `function_call`        | Tool/function invocation request |
| `function_call_output` | Tool execution result            |
| `custom`               | Diapason-specific extensions     |

```rust
use diapason_core::{OutputItem, MessageRole, ContentPart, ItemStatus};

// Create an assistant message
let message = OutputItem::message(
    "msg_1",
    MessageRole::Assistant,
    vec![ContentPart::output_text("Hello, world!")],
);

// Create a function call
let function_call = OutputItem::function_call(
    "fc_1",
    "exec_command",
    serde_json::json!({"cmd": "sed -n '1,40p' src/main.rs"}),
);
```

## Response Object Structure

The `Response` object follows the Open Responses specification:

```rust
use diapason_core::{OpenResponse as Response, ResponseStatus};

let mut response = Response::new("resp_123", "gpt-5-mini");
assert_eq!(response.status, ResponseStatus::InProgress);
assert_eq!(response.object, "response");

// Mark as completed
response.complete();
assert_eq!(response.status, ResponseStatus::Completed);
assert!(response.completed_at.is_some());
```

Key fields:

- `id`: Unique response identifier
- `object`: Always `"response"`
- `created_at`: Unix timestamp (seconds)
- `completed_at`: Completion timestamp (if applicable)
- `status`: Current response status
- `model`: Model that generated the response
- `output`: Vector of output items
- `usage`: Token usage statistics
- `error`: Error details (if failed)

## Bridging Diapason Events

The `ResponseBuilder` bridges Diapason's internal `ThreadEvent` system to Open
Responses:

```rust
use diapason_core::{ResponseBuilder, VecStreamEmitter, StreamEventEmitter};
use diapason_exec_events::{ThreadEvent, ThreadStartedEvent, TurnCompletedEvent, Usage};

let mut builder = ResponseBuilder::new("gpt-5");
let mut emitter = VecStreamEmitter::new();

// Process Diapason events
builder.process_event(
    &ThreadEvent::ThreadStarted(ThreadStartedEvent {
        thread_id: "thread_1".to_string(),
    }),
    &mut emitter,
);

// Get the Open Responses response
let response = builder.response();
```

### Event Mapping

| Diapason Event  | Open Responses Event                                      |
| --------------- | --------------------------------------------------------- |
| `ThreadStarted` | `response.created` + `response.in_progress`               |
| `TurnCompleted` | `response.completed`                                      |
| `TurnFailed`    | `response.failed`                                         |
| `ItemStarted`   | `response.output_item.added`                              |
| `ItemUpdated`   | `response.output_text.delta` / `response.reasoning.delta` |
| `ItemCompleted` | `response.output_item.done`                               |

### Item Type Mapping

| Diapason Item          | Open Responses Type                           |
| ---------------------- | --------------------------------------------- |
| `AgentMessageItem`     | `message` (role: assistant)                   |
| `ReasoningItem`        | `reasoning`                                   |
| `ToolInvocationItem`   | `function_call`                               |
| `ToolOutputItem`       | `function_call_output`                        |
| `CommandExecutionItem` | `custom` (type: `diapason:command_execution`) |
| `McpToolCallItem`      | `function_call`                               |
| `FileChangeItem`       | `custom` (type: `diapason:file_change`)       |
| `WebSearchItem`        | `custom` (type: `diapason:web_search`)        |
| `ErrorItem`            | `custom` (type: `diapason:error`)             |

## Error Handling

Structured errors follow the Open Responses specification:

```rust
use diapason_core::{OpenResponseError, OpenResponseErrorType, OpenResponseErrorCode};

// Create an error with type and message
let error = OpenResponseError::invalid_param("model", "Invalid model ID")
    .with_code(OpenResponseErrorCode::InvalidModel);

assert_eq!(error.error_type, OpenResponseErrorType::InvalidRequest);
assert_eq!(error.param, Some("model".to_string()));
```

Error types:

- `server_error`: Internal server error
- `invalid_request`: Invalid request parameters
- `not_found`: Resource not found
- `model_error`: Model-specific error
- `too_many_requests`: Rate limit exceeded

## Extension Points

Diapason-specific extensions use the `diapason:` prefix:

```rust
use diapason_core::open_responses::{CustomItem, is_valid_extension_type};

// Validate extension type naming
assert!(is_valid_extension_type("diapason:file_change"));
assert!(is_valid_extension_type("acme:search_result"));
assert!(!is_valid_extension_type("file_change")); // Missing prefix

// Create a custom item
let custom = CustomItem::diapason(
    "custom_1",
    "file_change",
    serde_json::json!({
        "path": "src/main.rs",
        "kind": "update",
    }),
);
assert_eq!(custom.custom_type, "diapason:file_change");
```

## Usage Statistics

Token usage follows the Open Responses format:

```rust
use diapason_core::{OpenUsage, InputTokensDetails};

let usage = OpenUsage {
    input_tokens: 1000,
    output_tokens: 200,
    total_tokens: 1200,
    input_tokens_details: Some(InputTokensDetails {
        cached_tokens: Some(500),
        audio_tokens: None,
        text_tokens: None,
    }),
    output_tokens_details: None,
};

// Convert from Diapason's internal usage
use diapason_exec_events::Usage as ExecUsage;
let exec_usage = ExecUsage {
    input_tokens: 1000,
    cached_input_tokens: 500,
    output_tokens: 200,
};
let open_usage = OpenUsage::from_exec_usage(&exec_usage);
```

## Streaming Flow

The correct streaming event flow per the specification:

```
response.created
  └─> response.in_progress
        └─> response.output_item.added
              └─> response.content_part.added
                    └─> response.output_text.delta (repeated)
                          └─> response.output_text.done
                                └─> response.content_part.done
                                      └─> response.output_item.done
  └─> response.completed (or response.failed)
```

## Configuration

Enable Open Responses in your `config.toml`:

```toml
[agent.open_responses]
# Enable Open Responses specification conformance layer
# Default: false (opt-in feature)
enabled = true

# Emit Open Responses events to the event sink
# (response.created, response.output_item.added, response.output_text.delta, etc.)
emit_events = true

# Include Diapason extension items (diapason:file_change, diapason:web_search, etc.)
include_extensions = true

# Map internal tool calls to Open Responses function_call items
map_tool_calls = true

# Include reasoning items in Open Responses output
include_reasoning = true
```

### Configuration Options

| Option               | Default | Description                               |
| -------------------- | ------- | ----------------------------------------- |
| `enabled`            | `false` | Enable the Open Responses layer (opt-in)  |
| `emit_events`        | `true`  | Emit semantic streaming events            |
| `include_extensions` | `true`  | Include Diapason-specific extension items |
| `map_tool_calls`     | `true`  | Map tool calls to `function_call` items   |
| `include_reasoning`  | `true`  | Include reasoning/thinking items          |

### Session output path

Interactive Open Responses exports are derived from the canonical session
stream. When file output is enabled, the JSONL stream is written to
`<state>/workspaces/<workspace-hash>/sessions/<session_id>/derived/open-responses.jsonl`
and the authoritative source remains
`<state>/workspaces/<workspace-hash>/sessions/<session_id>/events.jsonl`.

There is no default separate Open Responses artifact; exports are derived
beside the workspace-scoped canonical session stream.

### Programmatic Integration

```rust
use diapason_core::{OpenResponsesIntegration, OpenResponsesCallback};
use diapason_config::OpenResponsesConfig;
use std::sync::{Arc, Mutex};

// Create integration with config
let config = OpenResponsesConfig {
    enabled: true,
    ..Default::default()
};
let mut integration = OpenResponsesIntegration::new(config);

// Set up a callback for events
let callback: OpenResponsesCallback = Arc::new(Mutex::new(Box::new(|event| {
    println!("Open Responses event: {:?}", event.event_type());
})));
integration.set_callback(callback);

// Start a response session
integration.start_response("gpt-5");

// Process Diapason events (automatically converts to Open Responses)
// integration.process_event(&thread_event);

// Get the final response
if let Some(response) = integration.finish_response() {
    println!("Response completed: {}", response.id);
}
```

## Compliance Testing

Diapason includes a comprehensive compliance test suite to ensure strict
adherence to the Open Responses specification. This suite validates:

- **Object Validity**: Verifies that `Response` and `Request` objects contain
  all mandatory fields and follow the correct JSON schema.
- **State Machine Transitions**: Validates that items follow the state machine
  lifecycle (e.g., `in_progress` -> `completed`).
- **Streaming Event Sequences**: Ensures events are emitted in the correct
  order (e.g., `response.created` before `response.output_item.added`).
- **Extension Prefixing**: Validates that all custom items use the required
  `vendor:name` prefixing convention.
- **Agentic Loops**: Verifies proper mapping of tool calls and reasoning items.

### Running Compliance Tests

To run the Open Responses compliance test suite:

```bash
cargo test --test open_responses_compliance
```

The test source is located at `tests/open_responses_compliance.rs`.

## References

- [Open Responses Specification](https://www.openresponses.org/specification)
- [Open Responses API Reference](https://www.openresponses.org/reference)
- [Open Responses Governance](https://www.openresponses.org/governance)
