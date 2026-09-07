# ACP (Agent Client Protocol) Integration Guide

## Overview

VT Code exposes two ACP-related surfaces. The current integration is a
stdio-based ACP server for editor clients such as Zed. It negotiates the ACP
capabilities during `initialize`, serves sessions, and sends `session/update`
notifications over the protocol connection. This is the integration described
in the [Zed ACP guide](../guides/zed-acp.md).

This document also retains the original REST-based `AcpClient` reference. That
client is a legacy inter-agent API and is not the transport used by the current
ACP server; its sections are explicitly marked below.

**Key Features:**

-   stdio ACP server with `session/new`, `session/prompt`, and session resume
    support
-   capability negotiation for usage, task progress, and subagent management
-   streaming message, thought, and tool updates through `session/update`
-   the legacy REST client and its registry remain available for existing
    inter-agent callers

## Current ACP server

### Initialization and capability negotiation

The server responds to ACP `initialize` with protocol version 1 and advertises
the standard session capabilities it implements. It always includes the Lody
extension capability `_meta.lody.usage = { "version": 1 }`. Usage notifications
are sent only when the provider response includes usage data.

When the session has a subagent controller, the response additionally includes
`_meta.lody.subagents` version 1 with `lifecycle`, `list`, `cancel`, and `output`
set to `true`. If background subagents are enabled, it also includes
`_meta.lody.tasks = { "version": 1, "background": true }`. These extension
capabilities are conditional; a client must not assume that subagent
management or background tasks are available when they are absent from the
handshake.

Subagent and background-process progress uses ordinary ACP `session/update`
tool calls and tool-call updates. The update carries the task description and
status in the standard fields, while `meta.lody.task` contains the additional
Lody task snapshot (for example task ID, kind, actor, timestamps, summary, and
error details).

The following Lody extension requests are available when subagent management
was advertised:

- `_lody/subagents/list` lists the caller session's owned tasks;
- `_lody/subagents/cancel` requests cancellation of an owned task; and
- `_lody/subagents/output` returns a bounded output tail for an owned task.

Usage is reported with the `_lody/session/usage_update` extension notification.
Its parameters contain `sessionId`, a `usage` object, and `modelUsage`, keyed by
model name. The notification represents the usage delta for one provider
response, not a running total. The normalized usage object may contain:

- `inputTokens`, `outputTokens`, `cacheReadInputTokens`, and
  `cacheCreationInputTokens`;
- `reasoningOutputTokens`, when the provider reports reasoning usage (including
  the nested `completion_tokens_details.reasoning_tokens` field used by
  Baseten's OpenAI-compatible responses). `outputTokens` is the visible output
  portion after reasoning tokens are split out;
- `contextWindow`, from the resolved provider/model profile; and
- `costUSD`, only when opt-in custom-provider pricing resolves both input and
  output rates. Pricing is configured in USD per million tokens, with optional
  cache-read and cache-write rates.

Automatic context compaction is advertised as `_meta.lody.compaction = {
"version": 1 }`. Each compaction is represented by standard ACP
`tool_call`/`tool_call_update` session updates. The updates use a stable tool
call ID and carry `_meta.lody.activity` with the activity kind, token counts,
duration, and any failure reason; no private ACP update type is required.

VT Code advertises push-only `_meta.lody.rateLimits = { "version": 1 }`.
Recognized HTTP 429 quota headers produce `_lody/rate_limits/update` snapshots
scoped to the provider and model. Complete nonzero limit/remaining pairs
produce `usedPercent` and the provider's documented window duration. Missing
reset times remain null. Limit-only headers, including Fireworks throughput
ceilings, retain their absolute values in `limitName` with empty `windows`.
There is no quota query method or inferred account identifier. Per-request
token counters, `Retry-After`, and other diagnostics remain in warning notices;
they are not added to usage deltas. See
[provider rate-limit headers](../development/provider-rate-limit-headers.md)
for configuration and retry semantics.

The current server is launched with `vtcode acp` and communicates over stdio;
it does not expose the legacy `/messages`, `/metadata`, or `/health` HTTP
endpoints described in the reference below.

## Legacy REST/client reference

The material in the remaining sections documents the original REST-based
`AcpClient`, agent registry, and MCP wrappers. Keep it for callers that still
use that API, but do not use it as the launch or wire-format documentation for
the current stdio ACP server.

## Legacy REST/client architecture

```

            VT Code Main Agent
  (Primary decision-maker & orchestrator)



         Three MCP Tools:

         acp_call             Call remote agents
         acp_discover         Discover agents
         acp_health           Monitor health



          vtcode-acp
           HTTP Communication Layer
           Agent Registry
           Message Handling
           Connection Management






 Agent A     Agent B   ...  Agent N

```

## Legacy REST/client module structure

### `vtcode-acp` Library

The ACP client library is located in `vtcode-acp/` and provides:

#### Core Modules

1. **`client.rs`** - HTTP-based ACP communication

    - `AcpClient`: Main client for agent communication
    - `AcpClientBuilder`: Fluent builder for client configuration
    - Methods: `call_sync()`, `call_async()`, `ping()`, `discover_agent()`

2. **`discovery.rs`** - Agent registry and discovery

    - `AgentRegistry`: In-memory registry of available agents
    - `AgentInfo`: Metadata about a registered agent
    - Methods: `register()`, `find()`, `find_by_capability()`, `list_online()`

3. **`messages.rs`** - ACP message types

    - `AcpMessage`: Core message envelope
    - `AcpRequest`: Request structure
    - `AcpResponse`: Response structure
    - `MessageType`: Enum for message types (Request, Response, Error)
    - `ResponseStatus`: Status codes (Success, Failed, Timeout, Partial)

4. **`error.rs`** - Error types
    - `AcpError`: Comprehensive error handling
    - `AcpResult<T>`: Standard result type for ACP operations

### Agent Tool Integration

Three MCP tools expose ACP functionality to the main agent (now in `vtcode-core`):

1. **`acp_call`** - Inter-agent RPC calls

    - Params: `remote_agent_id`, `action`, `args`, `method` (sync/async)

2. **`acp_discover`** - Agent discovery

    - Modes: `list_all`, `list_online`, `by_capability`, `by_id`

3. **`acp_health`** - Health monitoring
    - Checks agent liveness via ping

## Usage Examples

### 1. Discovering Agents

```json
{
    "tool": "acp_discover",
    "input": {
        "mode": "list_online"
    }
}
```

Response:

```json
{
    "agents": [
        {
            "id": "data-processor",
            "name": "Data Processor",
            "base_url": "http://localhost:8081",
            "capabilities": ["bash", "python"],
            "online": true
        }
    ],
    "count": 1
}
```

### 2. Finding Agents by Capability

```json
{
    "tool": "acp_discover",
    "input": {
        "mode": "by_capability",
        "capability": "python"
    }
}
```

### 3. Calling a Remote Agent

```json
{
    "tool": "acp_call",
    "input": {
        "remote_agent_id": "data-processor",
        "action": "execute_script",
        "args": {
            "script": "import json; print(json.dumps({'status': 'ok'}))"
        },
        "method": "sync"
    }
}
```

### 4. Async Agent Call

```json
{
    "tool": "acp_call",
    "input": {
        "remote_agent_id": "long-runner",
        "action": "train_model",
        "args": {
            "epochs": 100
        },
        "method": "async"
    }
}
```

Returns immediately with `message_id`:

```json
{
    "message_id": "uuid-string",
    "status": "queued",
    "remote_agent_id": "long-runner",
    "action": "train_model"
}
```

### 5. Health Check

```json
{
    "tool": "acp_health",
    "input": {
        "agent_id": "data-processor"
    }
}
```

## Initialization

### In Rust Code

```rust
use vtcode_acp::{AcpClient, AgentInfo};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create client
    let client = AcpClient::new("my-agent".to_string())?;

    // Register a remote agent
    let agent = AgentInfo {
        id: "remote-agent".to_string(),
        name: "Remote Agent".to_string(),
        base_url: "http://localhost:8081".to_string(),
        description: Some("A sample remote agent".to_string()),
        capabilities: vec!["tool1".to_string(), "tool2".to_string()],
        metadata: Default::default(),
        online: true,
        last_seen: None,
    };

    client.registry().register(agent).await?;

    // Call the remote agent
    let result = client.call_sync(
        "remote-agent",
        "some_action".to_string(),
        serde_json::json!({"param": "value"}),
    ).await?;

    println!("Result: {}", result);
    Ok(())
}
```

### Via MCP Tools (Recommended)

The main agent simply uses the three MCP tools (`acp_call`, `acp_discover`, `acp_health`) when calling remote agents. The ACP client is initialized automatically during agent startup.

## Message Protocol

### Request Format

```json
{
    "id": "uuid",
    "type": "request",
    "sender": "vtcode",
    "recipient": "remote-agent",
    "content": {
        "action": "execute_tool",
        "args": {
            /* tool-specific args */
        },
        "sync": true,
        "timeout_secs": 30
    },
    "timestamp": "2024-01-01T12:00:00Z",
    "correlation_id": null
}
```

### Response Format

```json
{
    "id": "uuid",
    "type": "response",
    "sender": "remote-agent",
    "recipient": "vtcode",
    "content": {
        "status": "success",
        "result": {
            /* execution result */
        },
        "execution_time_ms": 245
    },
    "timestamp": "2024-01-01T12:00:00Z",
    "correlation_id": "original-request-id"
}
```

### Error Response

```json
{
    "id": "uuid",
    "type": "error",
    "sender": "remote-agent",
    "recipient": "vtcode",
    "content": {
        "code": "INVALID_ACTION",
        "message": "Unknown action: invalid_tool",
        "details": null
    },
    "timestamp": "2024-01-01T12:00:00Z",
    "correlation_id": "original-request-id"
}
```

## HTTP Endpoints (Remote Agent Requirements)

For an agent to be discoverable and callable via ACP, it must implement:

### POST `/messages`

Receive and process ACP requests.

**Request:**

```json
{
    /* ACP message */
}
```

**Response:**

```json
{
    /* ACP response */
}
```

### GET `/metadata`

Return agent metadata for discovery.

**Response:**

```json
{
    "id": "agent-id",
    "name": "Agent Name",
    "base_url": "http://localhost:8080",
    "description": "Agent description",
    "capabilities": ["action1", "action2"],
    "metadata": {},
    "online": true,
    "last_seen": "2024-01-01T12:00:00Z"
}
```

### GET `/health`

Health check endpoint.

**Response:**

```json
{
    "status": "ok",
    "timestamp": "2024-01-01T12:00:00Z"
}
```

## Configuration

Agent registry can be configured via `vtcode.toml`:

```toml
[acp]
local_agent_id = "vtcode-instance-1"
timeout_secs = 30

# Pre-registered agents
[[acp.agents]]
id = "data-processor"
name = "Data Processor"
base_url = "http://localhost:8081"
capabilities = ["bash", "python"]

[[acp.agents]]
id = "model-trainer"
name = "Model Trainer"
base_url = "http://localhost:8082"
capabilities = ["tensorflow", "pytorch"]
```

## Performance Considerations

### Synchronous Calls

-   Blocks main agent until response received
-   Best for short-running tasks (<5 seconds)
-   Recommended for control flow decisions

### Asynchronous Calls

-   Returns immediately with `message_id`
-   Main agent continues processing
-   Best for long-running tasks (>5 seconds)
-   Main agent must poll or subscribe for updates

### Timeout Handling

-   Default timeout: 30 seconds
-   Configurable per request
-   Async calls may timeout gracefully

### Registry Caching

-   Agent registry is in-memory
-   Agents stay registered until explicitly unregistered
-   Status updates via `update_status()` method
-   Health check marks agents online/offline

## Error Handling

Common error scenarios:

```rust
// Agent not found
AcpError::AgentNotFound("agent-id".to_string())

// Network/connection failure
AcpError::NetworkError("Connection refused".to_string())

// Remote agent returned error
AcpError::RemoteError {
    agent_id: "remote-agent".to_string(),
    message: "Action not supported".to_string(),
    code: Some(400),
}

// Request timeout
AcpError::Timeout("Request exceeded 30s timeout".to_string())

// Message serialization failed
AcpError::SerializationError("Invalid JSON".to_string())
```

## Roadmap

Planned enhancements:

-   [ ] Agent authentication (JWT/mutual TLS)
-   [ ] Message encryption
-   [ ] Decentralized agent discovery
-   [ ] Agent service mesh integration
-   [ ] Distributed tracing with OpenTelemetry
-   [ ] Agent metrics collection
-   [ ] Message queuing for resilience
-   [ ] Retry policies and circuit breakers

## See Also

-   [ACP Official Spec](https://agentcommunicationprotocol.dev/)
-   [MCP Integration Guide](../guides/mcp-integration.md)
-   [vtcode Configuration](../config/config.md)
