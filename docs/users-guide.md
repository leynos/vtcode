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
