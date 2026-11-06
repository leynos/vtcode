# Diapason User Guide

- [Getting started](./user-guide/getting-started.md)
- [Configuration](./config/config.md)
- [Zed ACP setup and sessions](./guides/zed-acp.md)
- [Patch safety: file versions and no-op guards](./guides/patch-safety.md)
- [Permissions](./guides/permissions.md)
- [MCP integration](./mcp/MCP_INTEGRATION_GUIDE.md)
- [User data directories](./guides/user-data-directories.md)
- [0.157.0 release notes](./releases/0.157.0.md) - Breaking API renames, the
  `--no-colour` flag, and configuration spelling aliases.

## ACP usage, compaction, and subagents

The Zed ACP integration advertises its Lody extensions during `initialize` and
falls back cleanly when a client ignores them. The
[Zed ACP guide](./guides/zed-acp.md) covers setup and session behaviour, and the
[ACP integration guide](./acp/ACP_INTEGRATION.md) documents the wire contracts.

- **Usage and cost.** `_lody/session/usage_update` reports per-response input,
  output, cache, and reasoning token counts. `contextWindow` comes from the
  resolved model profile. `costUSD` appears whenever that profile supplies both
  input and output rates: catalogue pricing for catalogue models, or the opt-in
  `custom_providers.*.pricing` block for custom providers. Cost normalization
  follows the backend the request was routed to, so an Anthropic Messages
  backend reports cache reads separately even under a custom provider name. An
  update is published only for a response whose provider reported usage;
  `cacheCreationInputTokens` and `reasoningOutputTokens` appear only when the
  provider reported them, `contextWindow` only when the resolved profile
  supplies one, and `costUSD` only when both rates are known.
- **Automatic compaction.** `_meta.lody.compaction` announces the capability.
  Each compaction is a standard tool call whose terminal update carries
  `_meta.lody.activity` with the `compacted` outcome, token counts, duration,
  and any failure reason.
- **Subagent management.** `_lody/subagents/list`, `_lody/subagents/cancel`, and
  `_lody/subagents/output` act only on tasks owned by the calling session.
  Progress arrives as standard ACP tool calls with `_meta.lody.task`.
- **Rate limits.** A provider rate limit produces a warning notice. Each part
  is reported only when it applies: the HTTP status and the provider's
  `Retry-After` value only when the provider supplied them, and the retry delay
  only when VT Code scheduled a further attempt. When no further attempt is
  scheduled the notice says so. VT Code does not invent quota windows, reset
  times, or statuses.
- **Custom-provider pricing.** Opt in per provider or per profile under
  `custom_providers.*.pricing`; rates are USD per million tokens. See the
  [configuration guide](./config/config.md).

## Patch safety in brief

`read_file` returns a whole-file `content_hash` alongside the text, whenever
the content can be certified against the bytes on disk. Truncated responses,
unsaved buffer content that diverges from disk, and append-only output that
grows mid-read return no hash rather than one the model could not rely on.
Passing a returned hash back to `apply_patch` as `expected_content_hash` makes
the edit conditional on the file still being the version the model read.

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
the sequence.

Rejections are bounded the same way. Two identical failures are permitted, and
the third identical request is refused with
`reason: "repeated_identical_failure"` before any file is touched. That bound
covers rejections the no-op ladder never observes, such as an undecodable or
unparsable patch, a stale `expected_content_hash`, a missing source file, and a
detected conflict.

The full behaviour, including how conflict and stale-file details reach an ACP
client, is in the [patch safety guide](./guides/patch-safety.md).

## ACP recovery in brief

A tool round can be interrupted after the model has asked for tools but before
every call has a result, for example when the editor or process exits mid-turn.
Loading or resuming such a session repairs the history instead of replaying it:

- Every unresolved call gets an explicit incomplete result, whose text says the
  call was not replayed and that its side effects are uncertain. Verify the
  workspace and resubmit only the work that is still needed.
- A terminal result that belongs unambiguously to an earlier call is moved back
  beside that call rather than duplicated.
- Duplicate, conflicting, or orphaned results make the history ambiguous. The
  archive is left untouched and the session is refused rather than resumed with
  a guess.
- Recovery writes a checkpoint before the session becomes visible, so an
  uncertain history cannot be resumed half-repaired.

Interrupted recovery never executes a tool a second time, because the original
call may already have changed files. The mechanics, including `write_stdin`
partial-state reporting and single-turn prompt admission, are in the
[Zed ACP guide](./guides/zed-acp.md) and the
[ACP runtime guide](./development/acp-runtime.md).

### Stale `write_stdin` sessions

An exec session identifier is process-local. It stops being valid when the
command exits, the session is closed, or Diapason restarts. Continuing a stale
identifier returns a non-retryable `ResourceNotFound` error with
`reason: "stale_exec_session"`; start a new `exec_command` and use the session
identifier it returns.

If the session was lost after `write_stdin` delivered its input, the error also
reports `input_delivered: true` and `partial_state_possible: true`. The bytes
may already have been consumed, so verify the workspace before repeating the
work. A failure before the input was written sets neither flag.

## Lody ACP extensions in brief

`diapason acp` serves the Agent Client Protocol over stdio. During
`initialize`, Diapason advertises Lody extensions under `_meta.lody`: `usage`
is always present, and subagent `lifecycle`, `list`, `cancel`, and `output` are
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
to streamed usage with `supports_stream_usage`; Diapason only requests it when
the provider declares that support.

For the full contract, see
[ACP capability negotiation and Lody extensions](./guides/zed-acp.md#acp-capability-negotiation-and-lody-extensions)
and the [ACP runtime guide](./development/acp-runtime.md).
