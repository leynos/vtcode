# Integrate PR #103 provider retry review repairs

This ExecPlan integrates the three frozen PR #103 provider-review batches on
the exact source head `f63adf8f197eb7a33d8438870bc359383a65e201`. It is a
living record: update its progress, decisions, verification, and outcomes as
the bounded work moves through the shared serial runner.

## Purpose and scope

The integrated plateau must make retry timing finite and deterministic,
prevent provider response-body data from entering error metadata, keep Gemini
quota projection explicitly empty, and make rate-limit header defaults easier
to validate without changing their public configuration path.

This worktree owns only:

- `crates/codegen/vtcode-core/src/retry.rs`
- `crates/codegen/vtcode-core/src/retry_after.rs`
- `crates/codegen/vtcode-llm/src/providers/error_handling.rs`
- `crates/codegen/vtcode-config/src/core/custom_provider.rs`
- `crates/codegen/vtcode-config/src/core/mod.rs`
- `crates/codegen/vtcode-config/src/core/rate_limit_headers.rs`
- this plan.

Do not include the separately committed documentation repair `035f1979d`,
create a provider-path extraction, change provider wire formats, add a public
clock trait or dependency, or publish, review, or merge. The serial runner
owns all format, lint, test, build, CodeScene, and CodeRabbit commands.

## Approved behaviour

- Delta `Retry-After` values remain normal for valid small values and cap at a
  finite 24-hour duration for integer, fractional, and fractional-carry
  overflow forms.
- HTTP-date `Retry-After` values influence the current retry decision only;
  they never become a retained provider floor for the next headerless retry.
- Whole reset seconds use strict ASCII-digit grammar.
- Error metadata has no provider body message, and Gemini explicitly maps no
  Baseten-compatible quota headers while retaining generic `Retry-After`.
- `RateLimitHeaderConfig` moves into a private sibling module, remains
  publicly re-exported through the existing configuration path, and derives
  field-dependent default, inheritance, and validation operations from one
  field list. Provider selection remains an ordinary inherent implementation.
- Anthropic defaults map only numeric quota headers; its RFC 3339 reset header
  is not treated as seconds.

## Milestones

### M1: Compose frozen source packets

Apply the retry batch, privacy/Gemini batch, and configuration batch to this
clean worktree. Reconcile the shared `error_handling.rs` edits without changing
either accepted invariant. Inspect all source manifests and record the unified
file list before requesting validation.

Acceptance: the diff has exactly the six implementation paths above plus this
plan; no source worktree is modified.

### M2: Validate the integrated provider contracts

The shared serial runner executes the focused selectors from all three source
packets before broader required gates. It runs no CodeRabbit request until all
deterministic checks are green.

Acceptance: retry, Gemini/privacy, and config-contract selections all pass
from the same integrated tree.

### M3: Freeze and hand off

After the shared runner reports green evidence, commit the integrated plateau
with a Conventional Commit message through a temporary file. The stack owner
will decide rebasing, publication, review replies, and merge.

## Verification plan

Run through the serial runner only:

```sh
RUSTFLAGS='-D warnings' cargo nextest run --locked -p vtcode-core --jobs 1 \
  -E 'test(retry_after_delta_seconds_cap_huge_values) | '\
'test(retry_after_http_date_is_identified_without_treating_'\
'delta_as_absolute) | '\
'test(retry_backoff_does_not_retain_an_http_date_floor)'
RUSTFLAGS='-D warnings' cargo nextest run --locked -p vtcode-llm --jobs 1 \
  -E 'test(signed_reset_after_seconds_are_rejected) | '\
'test(error_metadata_from_headers_does_not_retain_provider_body) | '\
'test(gemini_http_errors_keep_retry_after_without_baseten_quota_metadata)'
RUSTFLAGS='-D warnings' cargo nextest run --locked -p vtcode-config --jobs 1 \
  -E 'test(rate_limit_header_defaults_keep_the_serialized_baseten_shape) | '\
'test(provider_default_header_mappings_are_selected_case_insensitively) | '\
'test(anthropic_defaults_map_numeric_headers_without_reset_timestamp) | '\
'test(rate_limit_header_validation_rejects_invalid_names) | '\
'test(explicit_header_mapping_wins_over_provider_defaults)'
```

Then run the repository-required sequential gates. The runner must verify the
integrated source still generates the existing configuration schema and that
no retained HTTP-date floor or nonnumeric reset value escapes the focused
contracts.

## Progress

- 2026-09-08: Created a dedicated branch and worktree at the exact PR #103
  head. The three source batches remain unmodified evidence worktrees.
- 2026-09-08: Began source-manifest and shared-hunk review. An artisan maps
  the `error_handling.rs` composition before it is applied.
- 2026-09-08: Applied B1 first, then Batch A, and copied B2's new sibling
  module from its frozen worktree. The artisan confirmed the B1 and A hunks
  are independent: B1 owns Gemini/error-metadata sites, while A owns strict
  reset parsing. The integrated candidate now has the six approved source
  paths plus this plan and remains uncommitted pending shared validation.

## Decisions and risks

- Keep every added helper private. The explicit `SystemTime` retry seam is
  deterministic test support, not a general clock abstraction.
- Preserve the error body only in the provider error path, never in exported
  metadata. The Gemini empty mapping prevents accidental adoption of another
  provider's quota vocabulary.
- The main integration risk is the shared error helper. Compose its privacy,
  strict-reset, and Gemini changes in one file, then rely on all three focused
  selectors to prove that one invariant did not mask another.
- The configuration extraction may preserve serde/schema behaviour only if
  derives, field visibility, defaults, and old re-export routes remain exact.
- The composition order is B1, Batch A, then B2. B1 prevents generic
  Baseten mappings from becoming Gemini quota metadata before the retry batch
  validates its unchanged `Retry-After` path.

## Outcome and retrospective

Pending source composition and shared validation. No commit, push, comments,
or hosted action has occurred from this integration worktree.
