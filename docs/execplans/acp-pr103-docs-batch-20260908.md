# Correct PR #103 rate-limit documentation

## Purpose and scope

This plan resolves the four documentation findings on PR #103 commit
`f63adf8f197eb7a33d8438870bc359383a65e201`. It keeps the ACP quick reference,
the configuration field reference, and the provider-header guide consistent
with the implemented rate-limit behaviour. It does not change Rust code,
configuration, tests, or provider semantics.

The source findings are CodeRabbit `3952440917`, `3952440923`, `3952440926`,
and `3952440929`. Their full text and verified disposition are retained in
`/tmp/acp-pr103-review-intake-f63adf8.md`.

## Constraints

The batch owns these files:

- `docs/acp/ACP_QUICK_REFERENCE.md`
- `docs/config/CONFIG_FIELD_REFERENCE.md`
- `docs/development/provider-rate-limit-headers.md`
- `scripts/generate_config_field_reference.py`
- `scripts/tests/test_generate_config_field_reference.py`
- this plan

The branch begins at PR #103 head `f63adf8f197eb7a33d8438870bc359383a65e201`.
The parent runner owns all validation. No gate, commit, push, review request,
or GitHub comment is performed in this batch before shared validation passes.

## Milestones

### M1: Verify the reported documentation defects

Read the exact review comments and the cited source. Confirm that the ACP
quick-reference paragraph combines unrelated compaction, rate-limit, and
capability material; that `reset_after_seconds` is optional in
`RateLimitHeaderConfig`; and that the guide contains the two en-GB corrections.

Acceptance evidence: each correction is traceable to the cited current source
and preserves the documented runtime behaviour.

### M2: Apply the smallest truthful prose changes

Split the ACP paragraph after activity details. Mark only
`reset_after_seconds` as optional. Use `case-insensitive` and `behavioural`.
Do not describe unimplemented provider mappings, changed retry behaviour, or
new configuration requirements.

Acceptance evidence: the diff changes only the four owned files and retains
all existing rate-limit details and links.

### M3: Shared validation and handoff

The shared serial gate runner will perform the applicable Markdown checks.
After it reports green evidence, commit the batch only with parent approval;
the stack lead owns publication and review replies.

## Progress

- 2026-09-08: M1 verified against the four GitHub review comments and current
  source at the exact PR head.
- 2026-09-08: M2 applied. The first shared Markdown pass found 26 lint issues
  in the ACP quick reference and provider guide, plus one generated-reference
  typo. The new paragraph and language corrections are not the cause of those
  baseline issues, but the batch owns the affected documentation paths.
- 2026-09-08: The approved prerequisite cleanup is limited to distinct ACP
  subsection headings, fence spacing and languages, one guide line wrap, and
  the generated-reference source/regeneration path. The expected regenerated
  reference refresh is about 167 lines because it brings the stale committed
  table back to the exact `f63adf8` schema.
- 2026-09-08: Parent approved a narrow stable-home rendering rule in the
  generator: actual home-directory prefixes render as `$HOME`, so local host
  roots do not enter the generated reference. A focused contract will cover
  two home roots and a non-home path. Generator iteration reuses its existing
  schema snapshot while the shared runner owns the build slot.
- 2026-09-08: An existing exact-worktree `schema_dump` binary captured
  unmodified JSON at `/tmp/acp-pr103-f63-schema.json` for provisional offline
  regeneration. The snapshot rendered 946 fields and changed 89 inserted plus
  78 removed generated-document lines. This includes stale schema rows and is
  not manually reduced; a fresh Cargo schema/render byte comparison remains
  required from the shared runner.
- 2026-09-08: Shared validation passed the five focused Python tests, Ruff
  lint, snapshot-to-proposed-reference comparison, Markdown, and spelling.
  The runner then found only Ruff formatting and an external `--output` status
  display exception after a valid write. This bounded follow-up adds the exact
  formatter edits and a CLI regression using a temporary schema and output
  path; it does not alter generated content.
- 2026-09-08: The status-path repair preserves a repository-relative message
  for default output and reports an absolute external output path without
  calling `Path.relative_to` on an unrelated directory. The proposed source is
  frozen for the shared runner's focused six-test, Ruff, fresh-schema, and
  documentation gate packet.
- 2026-09-08: The CodeScene follow-up extracted default serialisation and
  truncation helpers, plus the field walker, branch handling, object maps, and
  array traversal into private functions. The generated 946-field reference
  remains untouched; shared gates must confirm byte identity and complexity
  thresholds before this repair is integrated.
- 2026-09-08: The traversal-state follow-up consolidates `root_schema` and
  `field_map` in one private `_FieldCollector`. Walker methods now take the
  current `FieldEntry` where its path and effective requiredness already carry
  the state, while the explicit depth argument preserves the recursion guard.
  The generated reference and focused tests remain unchanged. One shared pass
  must run the focused generator tests, Ruff format and lint, fresh locked
  schema regeneration with byte comparison, documentation checks, and the
  candidate CodeScene analysis.

## Decisions and discoveries

- `reset_after_seconds` remains optional because its Rust field is
  `Option<String>` with serde defaults. The parent object is also optional.
- The quick-reference wording about configured reset intervals is preserved;
  this batch changes paragraph structure only and does not assert new runtime
  behaviour.
- No Red-Green test stage applies because this is prose-only work. Markdown
  validation is the proportionate observable check.
- `CONFIG_FIELD_REFERENCE.md` is generated. Its two reported inaccuracies
  derive from `scripts/generate_config_field_reference.py`, so the generated
  file is not hand-edited. Parent approved the generator, its focused Python
  contract, and a regenerated reference as this batch's limited source
  ownership expansion.
- The generated table's requiredness means the complete dotted path is
  required. Generator traversal now composes every parent requirement with
  the child schema requirement, so an optional object cannot have an
  absolutely required child path.

## Risks and conformance

The only material risk is documentation drift. M1 anchors the correction to
the current configuration type and existing guide. The batch introduces no
public API, dependency, persistent format, or architecture change.

## Outcome and retrospective

Pending the one-pass focused generator and Python-format validation, fresh
Cargo schema regeneration, documentation checks, and candidate CodeScene
analysis, then commit integration already authorised by the user.
