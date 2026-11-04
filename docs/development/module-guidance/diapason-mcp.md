# diapason-mcp

> Retained module guidance, originally written for VTCode and updated to
> Diapason naming. Current path policy supersedes older details: user config is
> XDG `config.toml` plus `AGENTS.md`, durable logs/audit/sessions use XDG
> state, workspace data defaults to configurable `.diapason/`, and legacy
> `.vtcode/` paths are never read.

[Root AGENTS.md](../../../AGENTS.md) | Model Context Protocol client,
connection pooling, and tool discovery. Layer 1 crate -- depends on
diapason-config, diapason-commons, diapason-utility-tool-specs.

## Module Groups

| Area      | Modules                                                     |
| --------- | ----------------------------------------------------------- |
| Client    | `client.rs`, `provider.rs`, `rmcp_client.rs`                |
| Transport | `rmcp_transport.rs`, `connection_pool.rs`                   |
| Discovery | `tool_discovery.rs`, `tool_discovery_cache.rs`, `schema.rs` |
| Types     | `types.rs`, `traits.rs`, `errors.rs`, `enhanced_config.rs`  |
| Utils     | `utils.rs`                                                  |

## Rules

- `cli.rs` stays in diapason-core (depends on `crate::cli::input_hardening`).
- Re-export facade in diapason-core (`mcp/mod.rs`) must stay in sync.
- `rmcp_client` is `pub(crate)` -- not part of the public API.
- `convert_to_rmcp()` is `pub(crate)` -- internal JSON bridge.

## Gotchas

- `enhanced_config.rs` uses
  `#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]`.
- `rmcp-reqwest` is a renamed `reqwest` with rustls features -- not the same as
  the workspace `reqwest`.
- `DEFAULT_ENV_VARS` is platform-conditional (`#[cfg(unix)]` /
  `#[cfg(windows)]`).
- `McpSandboxContext` is optional for unsandboxed `McpClient::new`; session
  setup must pass it through initial, pooled, and reconnect stdio launches,
  whose stderr is bounded and redacted.
