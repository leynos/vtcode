# Apply-patch safety contract

This page documents the invariants the `apply_patch` preconditions rely on, for
anyone changing the read surface, the patch pipeline, or the edited-file
monitor. The user-facing behaviour is described in the
[patch safety guide](../guides/patch-safety.md).

## The problem being solved

An agent reads a file, reasons about it, and then emits a patch. Between the
read and the write the file may have changed, and the model has no way to
notice: its context still holds the version it read. Without a version, "the
patch no longer applies" is indistinguishable from "you read a stale file", so
the model tends to retry the same patch. Two independent guards break that loop.

1. A **content version** travels with every read and can be supplied back to
   `apply_patch` as a precondition.
2. A **no-op ladder** notices that a patch's *final state* equals the current
   state and refuses to accept the third identical attempt.

## Layer boundaries

The guards are deliberately split so each layer owns one decision:

| Layer                                             | Owns                                                      |
| ------------------------------------------------- | --------------------------------------------------------- |
| `tools::file_ops::read`                           | Sampling the version around the read window.              |
| `tools::registry::executors::patch_pipeline`      | Ordering: leases, snapshots, planning, guard observation. |
| `tools::registry::executors::patch_preconditions` | Building the structured rejection payloads.               |
| `tools::registry::patch_guard`                    | The no-op decision state machine and signature.           |
| `tools::handlers::apply_patch_handler`            | The Codex-style handler surface.                          |

`patch_guard` is pure: it takes parsed operations and before/after states and
returns a decision. It performs no I/O, which is what makes the signature
properties testable with proptests rather than fixtures.

## Invariant 1: the version describes what the model saw

`capture_read_content_hash_consistent` takes a `before` snapshot captured ahead
of the read and compares it with an `after` snapshot. The ordering matters for
files that change during the read: a hash taken afterwards can describe bytes
the model never received, which turns a valid patch into a spurious
`content_hash_mismatch` and invites a pointless regeneration.

When the two samples disagree the read fails, because the response text and the
version describe different revisions. An unversioned file cannot participate in
`expected_content_hash` checking, which is the correct outcome: refusing to
certify is safe, certifying the wrong content is not. Append-only tool output
is the deliberate exception — `allow_concurrent_growth` returns `Ok(None)`
there instead of failing, so a chunked read of live output keeps working while
remaining unversioned.

`record_read_content_hash_if_matches` covers the adjacent case where an ACP
client returns unsaved buffer content: the version is recorded only when the
on-disk bytes still match, so the edited-file baseline never claims a revision
that was never on disk.

A windowed ACP read is certified through
`tool_execution_local::windowed_disk_hash` instead of the returned-text hash.
It projects the same window from the on-disk bytes with
`vtcode_core::line_window` and certifies the whole-file hash only when that
projection equals the returned text, tolerating only a client that drops the
terminator of the final line. That difference is asymmetric: the returned text
may be one terminator shorter than the projection, never longer, and a returned
text that is itself newline-terminated must match exactly because it is a
complete set of lines. Trimming every trailing newline would accept extra blank
lines and certify a buffer that was never on disk, so no wider difference is
tolerated. The certified version therefore means "this window came from the
current on-disk bytes"; it does not claim the model saw the whole file, and it
does not detect unsaved edits outside the returned window. A non-UTF-8 file, or
a response that differs from the projected window, leaves the version absent,
and `apply_patch` treats absence as an omitted precondition.

### What a version is not

It is not a lock and not a lease. A version is read-then-checked immediately
before the write inside the patch pipeline's captured snapshot. Mutation
ordering is provided by the edited-file monitor's leases, which are acquired
before snapshots are captured. Conflating the two would create a false sense
that a versioned patch is serialized against other writers.

## Invariant 2: preconditions are checked before any mutation

`execute_apply_patch_internal` performs, in order:

1. decode and parse the patch,
2. resolve mutation paths, acquire leases,
3. capture each path's snapshot,
4. validate `expected_content_hash` against the captured snapshot,
5. detect per-operation conflicts,
6. plan writes, compute before/after states, observe the no-op guard,
7. apply.

Steps 4 and 6 both return tool outputs, not Rust errors, for rejected patches.
That is intentional: a rejected precondition is a normal tool result carrying a
`reason` and a `next_action`, and treating it as an `Err` would collapse the
model-visible detail into a generic failure.

The `apply_patch_handler.rs` path enforces the same hash precondition
independently via `validate_request_content_hash`, and it does so before the
orchestrator emits a mutation event, so a rejected patch never looks started.
Scope and mismatch rejections use the registry's shared builders
(`patch_precondition_scope_error`, `invalid_patch_precondition`), which is why
the add-only/multi-source distinction, the `source_file_count`, and a non-UTF-8
mismatch reported as `can_safely_rebase: false` with no anchor failures are
identical on both surfaces.

The no-op ladder and mutation leases are *not* available on the handler
surface: they are registry session state, and the handler has no registry
handle. The handler module documents this, and closing the gap is deferred
architecture work (#168) rather than an assumed guarantee.

### Read failures stay distinct from rebase verdicts

The rebase assessment is a pure function of content the caller has already read:
`assess_patch_rebase_with_content` takes the current text and returns
`(can_safely_rebase, anchor_failures)`. Reading is the caller's responsibility,
so a read failure must stay a Rust error rather than be reported as "cannot
rebase" — that would tell the model its anchors are wrong when the patch was
never assessed.

### Rebase assessment is strict and unique

The assessment renders through `render_patch_update_content_strict`
(`MatchPolicy::StrictUnique`), not the apply path's renderer. It accepts only a
byte-exact anchor that occurs exactly once in the same search window the
sequenced path uses. It deliberately does not try trailing-whitespace matching,
Unicode normalization, the trailing-empty-line retry, or semantic fallback, and
an anchor with two exact occurrences reports ambiguity instead of taking the
first.

Without that split the assessment would report `can_safely_rebase: true` for a
patch the apply path merely resolved *somehow*, which is the worst answer: the
model would keep a patch whose anchor never matched the file it was written
against. `false` never means "resend the same patch"; `next_action` still tells
the model to reread and regenerate.

Each reported failure carries a bounded `reason` — `ambiguous_anchor`,
`inexact_anchor`, `semantic_only_anchor`, or `missing_anchor` — alongside the
bounded `anchor`, `expected_excerpt`, and `current_excerpt`.

`apply_patch_handler::validate_request_content_hash` reads the file once,
hashes those bytes, and reuses the same buffer for the assessment. Reading a
second time would let a concurrent writer retarget a workspace symlink after
the containment check and quote an out-of-workspace file back through the
anchor failures. Where the registry supplies a snapshot that is not text there
is no content to assess, so `patch_preconditions` reports `(false, [])`; that
is a rejection the model can act on, not a masked I/O error.

## Invariant 3: planning follows the applicator's order

`planned_patch_writes` plans each operation against the *result* of the
preceding one, mirroring `Patch::apply`'s replay. If planning instead started
from the pre-patch snapshot, a patch that updates the same file twice would
compare its final state against the original and conclude it was a no-op, and
the guard would fire on productive work.

Any change to application order must be mirrored here. The two orderings
diverging is the most likely way to reintroduce false no-ops.

## Invariant 4: no-op identity is canonical

A no-op is only a no-op if `before == after` for every touched path. When that
holds, a signature is computed over:

- the operation count and each operation's kind,
- canonicalized source and destination paths,
- for updates, each hunk's change context, end-of-file flag, and lines tagged by
  kind,
- the before and after state of every touched path, sorted by path.

Length-prefixed field encoding prevents ambiguous concatenations. Three
consequences follow, and all are covered by proptests:

- Transport and path aliases collapse to one signature: CRLF versus LF, and
  `./x.txt` versus `x.txt`, describe the same request.
- Path order in the state list does not affect the signature.
- Operation order does not affect the signature for *independent* operations.
  `canonical_operation_order` orders operations by a deterministic topological
  walk: an edge runs from an earlier operation to a later one only when their
  touched path sets — source plus optional destination — intersect, and ties
  are broken by canonical source-path bytes then original index.

The dependence rule matters as much as the canonicalization. Two updates to the
same file replay in the supplied order, because the applicator applies them in
sequence and each sees the previous result. Sorting them would describe a patch
that was never executed. Only operations whose path sets are disjoint are free
to be reordered, which is exactly the set for which order carries no meaning.
Without this, two independent one-file no-ops hashed differently when the model
relisted them in the other order, so alternating representations restarted the
counter at occurrence 1 and the third-strike refusal was unreachable.

The ladder is `1` → `2` (warn, `retry_prohibited`) → `3` (block). The counter
increments only while the signature is unchanged; a different signature
restarts it at `1`, and a patch that actually changes a file resets it before
the signature is even computed.

## Observability

Both guards report through `metrics::patch_safety_metrics`, which keeps two
bounded counter sets and one timer:

- precondition outcomes: `matched`, `content_hash_mismatch`, `invalid_scope`,
  `malformed_hash`;
- no-op decisions: `execute`, `no_op_first`, `no_op_repeated`, `no_op_blocked`;
- `evaluations` plus `total_duration_ms`, from which `avg_duration_ms` is
  derived.

The snapshot appears as `patch_safety` in the metrics summary JSON and as
`vtcode_apply_patch_precondition_total{outcome=...}`,
`vtcode_apply_patch_no_op_total{decision=...}`,
`vtcode_apply_patch_evaluations_total`, and
`vtcode_apply_patch_avg_duration_ms` in the Prometheus export.

Every label is a fixed enum variant. Paths, patch signatures, patch text, and
error strings never become a label or a log value, so a session editing private
files can export this snapshot without leaking their content. Adding a
dimension means adding an enum variant and its `as_str` arm; a free-form string
label is not acceptable.

Each rejected precondition and each no-op decision also emits one
`tracing::info!` record carrying `operation`, `outcome` or `decision`,
`occurrence` where it applies, `session_id`, and `task_id`. Logs may carry the
bounded outcome, but not the rejection payload.

`PatchError`'s `Display` embeds the failing context or snippet, so an error
from the patch path must never be formatted into a log record. Use
`PatchError::reason_code()` — a fixed label such as `segment_not_found` — and,
when the size matters, `embedded_text_bytes()` for a numeric companion. The
conflict-preview failure in `patch_pipeline` follows this rule: it downcasts the
`anyhow` chain to recover the `PatchError` and logs `reason` plus
`text_bytes`, and a capture test asserts that a unique marker in the patch text
does not reach the tracing stream.

## Testing expectations

When changing any of the above, the following are the minimum checks:

- **Signature properties** — the proptests in `patch_guard` must still hold for
  alias collapse, path-order invariance, independent-operation-order
  invariance, and ladder reset.
- **Dependent operation order** — two updates to one file keep their supplied
  order; disjoint operations may be relisted either way.
- **Pipeline ordering** — a multi-update patch touching one file twice must
  execute, not be reported as a no-op, and alternating the order of two
  independent no-op updates must still reach occurrences 1, 2, and 3.
- **Rebase strictness** — a duplicate anchor, a whitespace-only match, and a
  Unicode-normalized-only match must all report `can_safely_rebase: false` with
  the matching `reason`, while an exact unique anchor still reports `true`.
- **Handler surface** — `apply_patch_handler` tests must cover a stale hash
  being rejected without mutating the file, a matching hash being allowed
  through to the write, the scope envelope for add-only and multi-source
  patches, and a binary mismatch reporting `can_safely_rebase: false` with no
  anchor failures.
- **Read certification** — a growing file must yield no version rather than an
  inconsistent one, and a windowed ACP read must certify the whole-file hash
  only when its text matches the on-disk projection.
- **Observability** — the `patch_safety` metrics test must keep asserting the
  summary fields, the Prometheus series, and that neither export can contain
  patch text or a hash.

Assert on the observable contract (file unchanged, `reason` present, occurrence
counted) rather than on internal state, so refactors inside the guard do not
require rewriting the tests.

## Related documentation

- [Patch safety guide](../guides/patch-safety.md) — user-facing behaviour.
- [Tool specifications](../tools/TOOL_SPECS.md) — the `apply_patch` surface.
- [ACP runtime](acp-runtime.md) — how these responses reach an ACP client.
