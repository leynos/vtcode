# Patch safety: file versions and repeated no-op guards

`apply_patch` refuses to mutate a file whose tracked version has changed since
it was read, and it stops accepting the same no-op patch over and over. Both
guards exist to keep one failure mode from wasting a turn: the agent
regenerating a patch against stale content, or resubmitting a patch that cannot
change anything.

Version checking is conditional rather than unconditional. A path is version
checked once a read has been recorded for it, at which point a subsequent
change is detected. A caller can additionally supply `expected_content_hash` as
an explicit precondition. When neither condition holds, the patch proceeds.

This guide covers what the two guards do, what each rejection looks like, and
how to recover.

## Why a file version is attached to every read

`read_file` returns a `content_hash` alongside the requested window. The hash
identifies the file content the model actually saw, so a later `apply_patch`
can prove it was written against that content and not against an older copy.

The hash always describes the whole file, including for a windowed read. Under
ACP, where the client may serve an unsaved editor buffer, the version is
returned only when the requested window matches what is on disk, so a buffer
that diverges from the file yields no `content_hash` rather than a hash for
bytes that were never saved.

The hash is sampled around the read itself rather than after it, so a file that
changes while it is being read does not silently produce a hash describing
bytes the model never received. If the content moved during the read, the read
fails rather than certifying a version. The one exception is append-only tool
output, which is expected to grow mid-read: those reads still return the chunk,
but without a `content_hash`, so they cannot be used as a precondition.

## Versioned patches

Pass the hash back as `expected_content_hash` so the patch applies only while
the file still matches:

```json
{
  "input": "*** Begin Patch\n*** Update File: src/app.rs\n@@ fn main() {\n-old\n+new\n*** End Patch\n",
  "expected_content_hash": "sha256:…"
}
```

Practical rules:

- `expected_content_hash` is only valid for a patch with exactly one
  pre-existing source file. Add-only patches have no prior version, and a
  multi-file patch has several, so split the patch by source file instead of
  guessing which hash to send. A single delete, or a single update that renames
  its file, also has exactly one source, so the argument is accepted there; for
  a rename the hash describes the source file, not the destination.
- Omit the argument when the guarantee is not required. It is optional, and
  unversioned patches behave as before.
- The hash is not a lock. It is a precondition: it is checked immediately
  before the patch is applied, and it only blocks a patch whose content has
  changed.

### When the version no longer matches

The patch is rejected before any file is touched. The response carries
`reason: "content_hash_mismatch"` together with the expected and current
hashes, the anchor failures that were detected, and `can_safely_rebase`.

- `can_safely_rebase: true` means every anchor matched the current content
  exactly once, so regenerating the patch against the fresh hash will normally
  apply cleanly.
- `can_safely_rebase: false` means the anchors moved, matched more than one
  location, or only matched after whitespace or character normalization. Each
  failure carries a `reason` (`missing_anchor`, `ambiguous_anchor`,
  `inexact_anchor`, or `semantic_only_anchor`) so the cause is visible. Reread
  the file and regenerate the hunk rather than adjusting the hash. A delete or
  a rename is not an assessable update, so it reports `false` with an empty
  `anchor_failures` list; reread the source and resend that operation.

The correct recovery in both cases is to reread the file and rebuild the patch
from what is actually there. Sending the new hash with the old hunk text only
moves the failure to the next step.

## Repeated no-op patches

A patch whose final file state is byte-identical to the current state changes
nothing. That is reported as success with `no_op: true`, because the request
did match reality: the content the model wanted is already on disk.

The first such result is informational. The second identical no-op in a row adds
`warning: true` and `retry_prohibited: true` with a `next_action` telling the
model not to resubmit the same payload. The third is refused with
`reason: "repeated_identical_no_op"`.

The ladder is keyed on a signature of the patch plus the before/after content
state of every touched path. It resets for any patch that actually changes a
file, and for any patch whose signature differs from the previous one — a
different patch, or the same patch against different file content.

Formatting-only variations of the same patch share one signature. Switching
line endings or writing `./file.txt` instead of `file.txt` does not reset the
ladder, because those describe the same request.

## Reading the responses

| Response                                  | Meaning                                                               | What to do                                                     |
| ----------------------------------------- | --------------------------------------------------------------------- | -------------------------------------------------------------- |
| `success`, no `no_op`                     | The patch applied.                                                    | Continue.                                                      |
| `success`, `no_op: true`, `occurrence: 1` | The target state already exists.                                      | Treat the work as done; do not resubmit.                       |
| `success`, `no_op: true`, `warning: true` | The same no-op repeated.                                              | Reread the files and change the approach.                      |
| `repeated_identical_no_op`                | The same no-op was offered three times.                               | Reread the files and submit a materially different patch.      |
| `content_hash_mismatch`                   | The file changed after it was read.                                   | Reread the file and regenerate the patch from the new content. |
| `invalid_precondition_scope`              | `expected_content_hash` was sent for an add-only or multi-file patch. | Split the patch so each one has a single source file.          |

The no-op ladder and the edited-file monitor's tracked-read state are per
session and held in memory. They are a guard rail against a looping turn, not a
durable audit trail, and they never survive a restart. A supplied
`expected_content_hash` is different: it is a stateless precondition compared
with the file's bytes on disk, so a hash still applies after a restart for as
long as the file itself is unchanged.

## Related documentation

- [Tool specifications](../tools/TOOL_SPECS.md) for the `apply_patch` surface.
- [Zed ACP guide](zed-acp.md) for the versioned-patch contract as it reaches an
  ACP client.
