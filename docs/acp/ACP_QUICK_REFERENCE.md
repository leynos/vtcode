# ACP Quick Reference

The current Diapason ACP integration is a stdio server. The REST `AcpClient`
snippets below are retained as a legacy client reference and do not describe
the server handshake or transport.

## Current ACP server

Launch the server with:

```bash
diapason acp
```

The server negotiates protocol version 1 during `initialize`. Its Lody usage
capability is always advertised as `_meta.lody.usage = { "version": 1 }`. When
a subagent controller is configured, the handshake also advertises
`_meta.lody.subagents` version 1 with `lifecycle`, `list`, `cancel`, and
`output` operations. The
`_meta.lody.tasks = { "version": 1, "background": true }` capability is
included only when background subagents are enabled.

Subagent and background-process progress is sent through standard ACP
`session/update` tool calls and tool-call updates. The standard task fields
carry the title and status; the additional task snapshot is in
`_meta.lody.task`.

The conditional Lody management requests are:

```text
_lody/subagents/list
_lody/subagents/cancel
_lody/subagents/output
```

All three require `sessionId`; `cancel` and `output` also require `taskId`.
Requests and responses use `camelCase` field names.

`_lody/subagents/list` accepts an optional `activeOnly` boolean (default
`false`) and returns the caller session's owned tasks:

<!-- tested-example: acp-quick-reference-subagent-list-request -->
```json
{
  "sessionId": "session-id",
  "activeOnly": false
}
```

<!-- tested-example: acp-quick-reference-subagent-list-response -->
```json
{
  "tasks": [
    {
      "taskId": "task-id",
      "description": "Review the parser",
      "status": "running",
      "agentId": "agent-session-id",
      "subagentType": "reviewer",
      "startedAtEpochSeconds": 1756339200.0,
      "endedAtEpochSeconds": null
    }
  ]
}
```

Each task carries `taskId`, `description`, `status`, `startedAtEpochSeconds`,
and `endedAtEpochSeconds` (a number, or `null` while the task is active), plus
optional `agentId`, `subagentType`, and `stopReason`. `status` is one of
`running`, `completed`, `failed`, or `killed`.

`_lody/subagents/cancel` requires `sessionId` and `taskId`; an optional
`reason` is accepted and currently ignored. It returns an empty object:

<!-- tested-example: acp-quick-reference-subagent-cancel-request -->
```json
{
  "sessionId": "session-id",
  "taskId": "task-id"
}
```

<!-- tested-example: acp-quick-reference-subagent-cancel-response -->
```json
{}
```

A task id the session does not own fails with error data
`{"reason": "unknown_task"}`.

`_lody/subagents/output` requires `sessionId` and `taskId` and accepts an
optional `tail` (default 200 lines, hard maximum 10,000). The response carries
the requested lines:

<!-- tested-example: acp-quick-reference-subagent-output-request -->
```json
{
  "sessionId": "session-id",
  "taskId": "task-id",
  "tail": 200
}
```

<!-- tested-example: acp-quick-reference-subagent-output-response -->
```json
{
  "output": "last lines of task output"
}
```

Reading output is a pure query: it sends no lifecycle update, restarts no
background process, and cannot be failed by an unrelated task. A background
task whose live exec session has exited is served from its recorded transcript
or archive.

The server reports per-response provider usage through the extension
notification `_lody/session/usage_update`. Its parameters contain:

<!-- tested-example: acp-quick-reference-usage-update -->
```json
{
  "sessionId": "session-id",
  "usage": {
    "inputTokens": 123,
    "outputTokens": 45,
    "cacheReadInputTokens": 0,
    "reasoningOutputTokens": 12,
    "contextWindow": 524288,
    "costUSD": 0.0000123
  },
  "modelUsage": {
    "model-name": {
      "inputTokens": 123,
      "outputTokens": 45,
      "cacheReadInputTokens": 0,
      "reasoningOutputTokens": 12,
      "contextWindow": 524288,
      "costUSD": 0.0000123
    }
  }
}
```

`modelUsage` is keyed by the response model, and each entry repeats the same
delta as `usage`. The values are deltas for one provider response, and no
notification is emitted when the provider supplies no usage data.

`inputTokens` is the full prompt count and already includes the tokens also
reported separately as `cacheReadInputTokens` and `cacheCreationInputTokens`,
whether the backend reports an inclusive or an exclusive prompt count. Adding
the three fields together double-counts the cached tokens.

`reasoningOutputTokens`, `contextWindow`, and `costUSD` are optional. The
visible `outputTokens` value excludes reasoning tokens when the provider
reports them; Baseten's nested `completion_tokens_details.reasoning_tokens`
field is preserved during normalization. `costUSD` is emitted when the resolved
model profile supplies both input and output rates (USD per million tokens):
catalogue pricing for catalogue models, or explicitly configured custom
provider pricing. Cache pricing is optional, and normalization follows the
selected backend's input-token semantics rather than the provider name.

Automatic context compaction is advertised as `_meta.lody.compaction` version 1
and is emitted through standard `tool_call` and `tool_call_update` updates. The
updates carry `_meta.lody.activity` for the `compacted` outcome, token counts,
and duration; a failed attempt adds a closed-set `failureReason` category
(`cancelled`, `timeout`, `admission_budget`, `pre_compact_hook`, `provider`)
rather than a raw diagnostic. Diapason does not advertise
`_meta.lody.rateLimits` without trustworthy quota state; HTTP 429 and
`Retry-After` are instead reported in warning notices and provider telemetry.

## Legacy REST ACP client reference

## Initialize ACP Client

```rust
use diapason_acp::AcpClient;

let client = AcpClient::new("my-agent".to_string())?;
let registry = client.registry();
```

## Register Remote Agent

```rust
use diapason_acp::AgentInfo;

let agent = AgentInfo {
    id: "remote-agent".to_string(),
    name: "Remote Agent".to_string(),
    base_url: "http://localhost:8081".to_string(),
    description: Some("Description".to_string()),
    capabilities: vec!["action1".to_string(), "action2".to_string()],
    metadata: Default::default(),
    online: true,
    last_seen: None,
};

registry.register(agent).await?;
```

## Discover Agents

```rust
// List all agents
let all = registry.list_all().await?;

// List online agents
let online = registry.list_online().await?;

// Find by capability
let agents = registry.find_by_capability("python").await?;

// Find by ID
let agent = registry.find("agent-id").await?;
```

## Call Remote Agent (Sync)

```rust
use serde_json::json;

let result = client.call_sync(
    "remote-agent",
    "execute".to_string(),
    json!({"param": "value"}),
).await?;

println!("Result: {}", result);
```

## Call Remote Agent (Async)

```rust
let message_id = client.call_async(
    "remote-agent",
    "long_task".to_string(),
    json!({"data": "..."}),
).await?;

// Store message_id for later polling
println!("Task queued: {}", message_id);
```

## Check Agent Health

```rust
let is_online = client.ping("remote-agent").await?;

if is_online {
    println!("Agent is reachable");
} else {
    println!("Agent is offline");
}
```

## Discover Agent Metadata

```rust
let agent_info = client.discover_agent("http://localhost:8081").await?;
println!("Agent: {} ({})", agent_info.name, agent_info.id);
println!("Capabilities: {:?}", agent_info.capabilities);
```

## Error Handling

```rust
use diapason_acp::AcpError;

match client.call_sync("id", "action".to_string(), json!({})).await {
    Ok(result) => println!("Success: {}", result),
    Err(AcpError::AgentNotFound(id)) => println!("Agent {} not found", id),
    Err(AcpError::NetworkError(e)) => println!("Network: {}", e),
    Err(AcpError::Timeout(e)) => println!("Timeout: {}", e),
    Err(AcpError::RemoteError { agent_id, message, code }) => {
        println!("Remote error from {}: {} ({:?})", agent_id, message, code);
    }
    Err(e) => println!("Error: {}", e),
}
```

## Message Types

```rust
use diapason_acp::AcpMessage;

// Create request
let msg = AcpMessage::request(
    "sender".to_string(),
    "recipient".to_string(),
    "action".to_string(),
    json!({"param": "value"}),
);

// Serialize
let json = msg.to_json()?;

// Deserialize
let msg = AcpMessage::from_json(&json)?;

// Create response
let response = AcpMessage::response(
    "agent-a".to_string(),
    "agent-b".to_string(),
    json!({"result": "data"}),
    "correlation-id".to_string(),
);

// Create error
let error = AcpMessage::error_response(
    "agent-a".to_string(),
    "agent-b".to_string(),
    "ERROR_CODE".to_string(),
    "Error message".to_string(),
    "correlation-id".to_string(),
);
```

## Update Agent Status

```rust
// Mark as offline
registry.update_status("agent-id", false).await?;

// Mark as online
registry.update_status("agent-id", true).await?;
```

## Registry Operations

```rust
// Count agents
let count = registry.count().await;

// Clear all agents
registry.clear().await;

// Unregister agent
registry.unregister("agent-id").await?;
```

## Custom Timeout

```rust
use std::time::Duration;
use diapason_acp::AcpClientBuilder;

let client = AcpClientBuilder::new("local-agent".to_string())
    .with_timeout(Duration::from_secs(60))
    .build()?;
```

## MCP Tool Usage (From Main Agent)

### Discover Agents

```json
{
  "tool": "acp_discover",
  "input": {
    "mode": "list_online"
  }
}
```

### Find by Capability

```json
{
  "tool": "acp_discover",
  "input": {
    "mode": "by_capability",
    "capability": "python"
  }
}
```

### Call Remote Agent (Sync)

```json
{
  "tool": "acp_call",
  "input": {
    "remote_agent_id": "data-processor",
    "action": "process",
    "args": {"input": "data"},
    "method": "sync"
  }
}
```

### Call Remote Agent (Async)

```json
{
  "tool": "acp_call",
  "input": {
    "remote_agent_id": "trainer",
    "action": "train_model",
    "args": {"epochs": 100},
    "method": "async"
  }
}
```

### Check Agent Health

```json
{
  "tool": "acp_health",
  "input": {
    "agent_id": "data-processor"
  }
}
```

## HTTP Endpoints (Remote Agent Must Implement)

### POST /messages

```
Request: AcpMessage
Response: AcpResponse
```

### GET /metadata

```
Response: AgentInfo
```

### GET /health

```
Response: "OK" or JSON status
```

## Environment Variables

```bash
# Enable tracing
RUST_LOG=diapason_acp=trace

# Or with all details
RUST_LOG=debug
```

## Common Patterns

### Master-Worker

```rust
// Master discovers worker agents
let workers = registry.find_by_capability("worker").await?;

// Distribute work across workers
for (i, worker) in workers.iter().enumerate() {
    let result = client.call_sync(
        &worker.id,
        "process".to_string(),
        json!({"data": format!("batch_{}", i)}),
    ).await?;
    println!("Worker {} result: {}", worker.id, result);
}
```

### Pipelined Processing

```rust
// Data processor
let data = client.call_sync(
    "processor",
    "clean".to_string(),
    json!({"raw": raw_data}),
).await?;

// Model trainer
let model = client.call_sync(
    "trainer",
    "train".to_string(),
    json!({"data": data}),
).await?;

// Report generator
let report = client.call_sync(
    "reporter",
    "generate".to_string(),
    json!({"model": model}),
).await?;
```

### Async Batch Processing

```rust
// Queue multiple jobs
let mut job_ids = vec![];

for batch in batches {
    let msg_id = client.call_async(
        "processor",
        "process_batch".to_string(),
        json!({"batch": batch}),
    ).await?;
    job_ids.push(msg_id);
}

// Later, poll for results
for job_id in job_ids {
    println!("Job {}: queued", job_id);
}
```

## Testing

```bash
# Run all ACP client tests
cargo test -p diapason-acp

# Run specific test
cargo test -p diapason-acp test_agent_registry

# Run with output
cargo test -p diapason-acp -- --nocapture

# Run example
cargo run --example acp_distributed_workflow
```

## Troubleshooting

### Agent Not Found

```
Error: Agent not found: agent-id
→ Check if agent was registered
→ Check registry.list_all() to see what's registered
```

### Network Error

```
Error: Network error: Connection refused
→ Check if remote agent is running
→ Verify base_url is correct
→ Check firewall/network connectivity
```

### Timeout

```
Error: Request exceeded 30s timeout
→ Increase timeout with AcpClientBuilder.with_timeout()
→ Check if remote agent is responding slowly
→ Consider using async call for long tasks
```

### Serialization Error

```
Error: Failed to parse response
→ Check remote agent is returning valid JSON
→ Verify message format matches ACP spec
```

## Documentation Links

- [Full ACP Integration Guide](./ACP_INTEGRATION.md)
- [Client Library README](../../diapason-acp/README.md)
- [ACP Specification](https://agentcommunicationprotocol.dev/)
