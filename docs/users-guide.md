# Diapason User Guide

- [Getting started](./user-guide/getting-started.md)
- [Configuration](./config/config.md)
- [Zed ACP setup and sessions](./guides/zed-acp.md)
- [Patch safety: file versions and no-op guards](./guides/patch-safety.md)
- [Permissions](./guides/permissions.md)
- [MCP integration](./mcp/MCP_INTEGRATION_GUIDE.md)
- [User data directories](./guides/user-data-directories.md)

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
