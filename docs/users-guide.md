# VT Code User Guide

- [Getting started](./user-guide/getting-started.md)
- [Configuration](./config/config.md)
- [Zed ACP setup and sessions](./guides/zed-acp.md)
- [Patch safety: file versions and no-op guards](./guides/patch-safety.md)
- [Permissions](./guides/permissions.md)
- [MCP integration](./mcp/MCP_INTEGRATION_GUIDE.md)
- [User data directories](./guides/user-data-directories.md)

## Patch safety in brief

`read_file` returns a whole-file `content_hash` alongside the text. Passing
that hash back to `apply_patch` as `expected_content_hash` makes the edit
conditional on the file still being the version the model read.

- If the hash matches, the patch applies normally.
- If the file changed, the patch is rejected before any write. The rejection
  carries `reason: "content_hash_mismatch"`, the supplied hash, the current
  hash, and a `next_action`. Reread the file and regenerate the patch against
  the new version; nothing on disk was touched.
- Omit `expected_content_hash` when the guarantee is not required. Add-only
  patches and patches with more than one source file must omit it; supplying it
  there returns `reason: "invalid_precondition_scope"`.

Independently of hashes, `apply_patch` compares each touched file's state
before and after the patch. A patch that would leave every file exactly as it
already is has no effect, so repeating the identical no-op is escalated rather
than silently accepted:

1. first attempt: reported as success with `"no_op": true` and
   `"occurrence": 1`,
2. second identical attempt: still accepted, but flagged with
   `"retry_prohibited": true` and a warning,
3. third identical attempt: refused with `reason: "repeated_identical_no_op"`.

Any patch that does change the files, or a materially different patch, resets
the sequence. The full behaviour, including how conflict and stale-file details
reach an ACP client, is in the [patch safety guide](./guides/patch-safety.md).

## Lody ACP extensions in brief

`vtcode acp` serves the Agent Client Protocol over stdio. During `initialize`,
VT Code advertises Lody extensions under `_meta.lody`: `usage` is always
present, and subagent `lifecycle`, `list`, `cancel`, and `output` are
advertised when a subagent controller exists for the launch workspace.
`_meta.lody.tasks.background` appears only when background subagents are
enabled. These capabilities describe the launch workspace. Management is
session-scoped, so a request can still fail with
`subagent_management_unavailable`.

Subagent and background-process lifecycle is published as standard ACP
`session/update` tool calls and tool-call updates, with the task snapshot under
`_meta.lody.task`. When management is available, the `_lody/subagents/list`,
`_lody/subagents/cancel`, and `_lody/subagents/output` requests operate only on
tasks owned by the calling session. Provider usage is reported through
`_lody/session/usage_update` as a per-response delta. Custom providers opt in
to streamed usage with `supports_stream_usage`; VT Code only requests it when
the provider declares that support.

For the full contract, see
[ACP capability negotiation and Lody extensions](./guides/zed-acp.md#acp-capability-negotiation-and-lody-extensions)
and the [ACP runtime guide](./development/acp-runtime.md).
