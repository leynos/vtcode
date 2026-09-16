# VT Code Async Architecture Documentation

## Quick Links

- **[Architecture Reference](./ASYNC_ARCHITECTURE.md)** - How the async
    system works

## TL;DR

The VT Code system has **100% async I/O operations**. All file operations use
`tokio::fs`, PTY operations use `tokio::task::spawn_blocking`, and HTTP
requests use `reqwest` async.

Authoritative session events use bounded backpressure and stop accepting new
events when persistence fails; diagnostic trajectory logs use bounded
best-effort buffering with periodic and shutdown flushes. See the
[async architecture guide](../guides/async-architecture.md) for the pipeline
decision rules and capacity policy.

## Architecture Overview

```
User Interface (TUI)
        ↓
Agent Turn Loop (Async)
        ↓
Tool Execution Pipeline (Async)
        ↓
Tool Registry (Async)
        ↓


PTY Operations    File Operations   HTTP Requests
(spawn_blocking)  (tokio::fs)      (reqwest async)
```

**All layers are fully async**

## Related Documentation

- [Main README](../../README.md)

______________________________________________________________________

**Status**: Complete
