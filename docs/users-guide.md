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

### Durable checkpoints before tool rounds

ACP requires a durable session checkpoint before every tool round. The
checkpoint records the assistant tool request and one incomplete result per
call, so an interrupted round can be repaired on load. When
`[history].persistence = "none"`, the checkpoint cannot be written and the
prompt fails with a checkpoint error before any tool executes. No tool side
effects occur in that case. Enable durable history, for example
`history.persistence = "file"`, to use ACP tool rounds. See
[durable session archives](./config/config.md#durable-session-archives).

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

### Recovery metrics

Recovery records bounded counters instead of identifiers, so an export can
never leak session identifiers, tool names, call IDs, or tool output. The
counters are agent-wide, not per session: every workspace registry the agent
builds records into the same collector, so events from any session accumulate
in the agent's snapshot. The `tool_recovery` object of the `MetricsSummary`
that collector produces holds one counter per recovery event:

<!-- tested-example: users-guide-tool-recovery-summary -->

```json
{
  "tool_recovery": {
    "staged_calls": 3,
    "replaced_calls": 3,
    "interrupted_calls": 1,
    "relocated_results": 1,
    "repaired_archives": 1,
    "ambiguous_histories": 1,
    "concurrent_prompt_rejections": 0,
    "stale_write_stdin_requests": 1,
    "checkpoint_failures": 0,
    "checkpoint_skips": 1,
    "duplicate_tool_call_batches": 1
  }
}
```

Prometheus exports the same counters as
`diapason_tool_recovery_total{event="…"}`. The `event` label is one of
`staged_call`, `replaced_call`, `interrupted_call`, `relocated_result`,
`repaired_archive`, `ambiguous_history`, `concurrent_prompt_rejection`,
`stale_write_stdin_request`, `checkpoint_failure`, `checkpoint_skipped`, and
`duplicate_tool_call_batch`.

Agents that embed Diapason can read the same vocabulary from
`diapason_core::metrics::{ToolRecoveryEvent, ToolRecoveryMetrics}`. Record an
event with `MetricsCollector::record_tool_recovery`, or a batch with
`record_tool_recovery_events`, rather than incrementing a private counter.

A host that builds more than one `ToolRegistry` can install one shared
collector with `ToolRegistry::set_metrics_collector`. The replacement affects
only the registry instance it is called on: counters already recorded in the
previous collector are not migrated, and components that cloned that `Arc` at
construction keep recording into it. Paths that read the collector through
`ToolRegistry::metrics_collector()` observe the new value. Install the shared
collector while the registry is being built, before dependent components
capture it.

### Checkpoint preparation API

`SessionArchive::prepare_checkpoint_snapshot` compacts a checkpoint payload and
returns it without writing anything.
`SessionArchive::write_prepared_checkpoint_async` then writes exactly that
payload to the archive path. Splitting the operation lets a caller inspect the
compacted snapshot, or reject it, before it replaces the archive on disk, so a
payload that lost required evidence cannot overwrite the copy an earlier
checkpoint made durable. Pass the snapshot returned by the first call straight
to the second; preparing it again would compile a different payload.

`prepare_checkpoint_snapshot` returns `None` when history persistence is
disabled. Callers that treat the checkpoint as required must report that as a
failure rather than writing unconditionally, which is why an ACP tool round
cannot run with `[history].persistence = "none"`. A caller that only records
transcript progress may treat `None` as a skipped, best-effort write.

### History mutation API

`ThreadRuntimeHandle::mutate_messages` applies a short, synchronous callback
while the session lock is held, and `replace_messages` swaps in a history that
does not depend on the current one. Both are the supported way to stage
incomplete tool results and replace them with terminal results. The callback
must not call back into the handle, await, or perform I/O, because it runs with
the history lock taken.
