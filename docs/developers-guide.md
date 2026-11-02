# VT Code Developer Guide

- [Development setup and workflows](./development/README.md)
- [Architecture](./ARCHITECTURE.md)
- [ACP runtime](./development/acp-runtime.md)
- [Custom-provider request policy](./development/CUSTOM_PROVIDER_REQUEST_POLICY.md)
- [Apply-patch safety contract](./development/patch-safety-contract.md)
- [Testing](./development/testing.md)
- [Configuration field reference](./config/CONFIG_FIELD_REFERENCE.md)

## ACP and Lody runtime

The [ACP runtime](./development/acp-runtime.md) guide describes the developer
surface behind the Zed integration:

- session-scoped subagent controller resolution, ownership, and descendant
  routing;
- lifecycle projection into standard ACP `session/update` messages;
- the `_lody/subagents/list`, `_lody/subagents/cancel`, and
  `_lody/subagents/output` management RPCs;
- `_lody/session/usage_update` publication; and
- the `supports_stream_usage` custom-provider opt-in.

The user-facing capability and extension contract is in the
[Zed ACP guide](./guides/zed-acp.md#acp-capability-negotiation-and-lody-extensions).
