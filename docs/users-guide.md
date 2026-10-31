# VT Code User Guide

- [Getting started](./user-guide/getting-started.md)
- [Configuration](./config/config.md)
- [Zed ACP setup and sessions](./guides/zed-acp.md)
- [Patch safety: file versions and no-op guards](./guides/patch-safety.md)
- [Permissions](./guides/permissions.md)
- [MCP integration](./mcp/MCP_INTEGRATION_GUIDE.md)
- [User data directories](./guides/user-data-directories.md)

## ACP usage, compaction, and subagents

The Zed ACP integration advertises its Lody extensions during `initialize` and
falls back cleanly when a client ignores them. The
[Zed ACP guide](./guides/zed-acp.md) covers setup and session behaviour, and the
[ACP integration guide](./acp/ACP_INTEGRATION.md) documents the wire contracts.

- **Usage and cost.** `_lody/session/usage_update` reports per-response input,
  output, cache, and reasoning token counts. `contextWindow` comes from the
  resolved model profile, and `costUSD` appears only when custom-provider
  pricing supplies both input and output rates.
- **Automatic compaction.** `_meta.lody.compaction` announces the capability.
  Each compaction is a standard tool call whose terminal update carries
  `_meta.lody.activity` with the `compacted` outcome, token counts, duration,
  and any failure reason.
- **Subagent management.** `_lody/subagents/list`, `_lody/subagents/cancel`, and
  `_lody/subagents/output` act only on tasks owned by the calling session.
  Progress arrives as standard ACP tool calls with `_meta.lody.task`.
- **Rate limits.** A provider HTTP 429 produces a warning notice carrying the
  provider's `Retry-After` value and the scheduled retry. VT Code does not
  invent quota windows or reset times.
- **Custom-provider pricing.** Opt in per provider or per profile under
  `custom_providers.*.pricing`; rates are USD per million tokens. See the
  [configuration guide](./config/config.md).

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
