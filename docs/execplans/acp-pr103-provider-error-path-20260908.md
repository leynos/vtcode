# PR103 provider error-path metadata

## Status

Execution authorised by the ACP hardening stack lead on 2026-09-08. The
baseline is `6bb55a65fd4dc261d0881deb5bda1c3cd7d1477f`; that plateau passed
focused deterministic gates, immutable CodeScene review, and CodeRabbit CLI.
This plan is in progress and may commit only after the serial gate runner
reports every applicable check green.

## Outcome

Provider error paths will return consistent, bounded metadata for real local
OpenAI and OpenRouter HTTP failures. Status, request identifier, Retry-After,
and explicitly configured numeric quota fields will survive the path. A unique
provider-body marker must not enter error metadata. Existing formatted error
messages and rate-limit classification stay unchanged.

## Scope and constraints

CR3952440889 permits a private rate-header parser module. CR3952440904 permits
one private OpenAI helper for ten non-rate metadata sites. CR3952440908 permits
one private OpenRouter provider-name/configuration helper. No public provider
trait, wire format, event type, paid provider probe, universal parser, or
new dependency is permitted. `openai/stream_decoder.rs` and
`shared/responses_reconciler.rs` belong to PR104 and are excluded.

Metadata privacy is narrower than outer error-message or ACP projection
privacy: this milestone proves only that metadata excludes the body marker.
Gemini helper semantics have no real caller in this scope and are not claimed as
end-to-end coverage. Anthropic numeric-header mapping remains proven by B2
configuration tests; native error-path coverage requires separate evidence or a
scope escalation.

OpenRouter 429 paths deliberately retain
`parse_api_error_with_headers`, whose established rate-error metadata contains
a sanitised diagnostic. Their regression assertions therefore prove provider,
status, request identifier, Retry-After, and configured quota metadata without
asserting that the body is absent. The body-free metadata assertion applies only
to the ten non-rate OpenAI paths using `error_metadata_from_headers`.

## Conformance basis

The ACP remediation programme requires bounded provider diagnostics and reliable
rate-limit metadata without a competing provider abstraction. The review items
above are the direct requirements. `LLMError` remains the existing error
contract, and `ThreadEvent` is untouched.

## Milestones

1. **Define executions that matter.** Add local Wiremock regressions before
   production consolidation where a path lacks coverage: OpenAI Responses
   buffered/streamed, Chat buffered/streamed including unsupported-Responses
   then failing-Chat fallback, and OpenRouter ordinary/fallback 429. A test
   proves status, request id, Retry-After, configured numeric quota fields, and
   body-marker absence from metadata. Red evidence is recorded only if the
   existing implementation fails the new concrete assertion; the pre-existing
   duplication is structural rather than an observable defect.
2. **Extract private header reduction.** Move response metadata and the final
   reset parser to `error_handling/rate_limit_headers.rs`; retain classification
   and `LLMError` shaping in `error_handling.rs`. Strict ASCII grammar,
   fractional ceiling rounding, overflow checks, and the 24-hour cap remain
   unchanged.
3. **Consolidate real callers.** Add the smallest private helpers on
   `OpenAIProvider` and `OpenRouterProvider`, migrate only the ten and three
   evidenced sites, and preserve rate-limit branches and error text.
4. **Prove and review.** The serial runner executes formatter, focused provider
   nextest selectors, changed-crate Clippy, Markdown, spelling, immutable
   CodeScene, then CodeRabbit CLI. Commit only on green. Hosted/PR review and
   publication stay with the stack lead.
5. **Isolate OpenAI metadata contracts.** Move only this milestone's OpenAI
   header-metadata fixtures into a sibling test module. Parameterize the
   legacy and normalized Responses stream forms through an explicit test-only
   mode while retaining both public entrypoints and their request assertions.
   Keep the five production error-helper inputs explicit: they describe the
   provider error boundary and must not be wrapped solely for a metric.
6. **Keep custom-provider tests navigable.** Split the extracted test suite
   into authentication, error-metadata, sampling, and streaming modules. The
   repository warns at 500 Rust lines; it does not impose a 400-line rule.
   Parameterized transport cases retain both buffered and streaming calls.

## Verification plan

The parser invariant is that only non-empty ASCII decimal values are accepted;
whole seconds and fractional milliseconds retain current truncation, ceiling,
cap, and overflow rules. The metadata invariant is that extracted fields come
only from allow-listed headers and never from the response body. The caller
invariant is that unsupported Responses fallback reports metadata from the
terminal Chat request, while rate-limit sites keep
`parse_api_error_with_headers`.

The tests rely on Wiremock's local listener, which existing helpers skip only
when listener binding is prohibited. Serial commands run sequentially:
`cargo fmt --check`; focused `cargo nextest run --locked -p vtcode-llm` OpenAI
and OpenRouter selectors; `cargo clippy --locked -p vtcode-llm --all-targets --
-D warnings`; Markdown and spelling; immutable CodeScene; then CodeRabbit CLI.

## Progress

- 2026-09-08: Created branch
  `review/acp-pr103-provider-error-path-20260908` at the cleared parser plateau.
- 2026-09-08: Began bounded private header-module extraction and OpenAI/
  OpenRouter caller reconnaissance. No gates have run for this milestone.
- 2026-09-08: Replaced the ten OpenAI non-rate metadata constructions and the
  three OpenRouter rate-parser literals with private helpers. Added local
  Wiremock coverage for buffered and stream OpenAI errors and ordinary and
  tool-fallback OpenRouter 429 errors. Validation remains delegated.
- 2026-09-08: Source inspection found no existing native Anthropic error-path
  fixture. The stack lead approved a test-file-only scope expansion, so a local
  `/messages` 429 regression now proves default numeric quota mapping and that
  the timestamp reset header remains unmapped.
- 2026-09-08: Expanded the OpenAI matrix after review found the initial two
  fixtures insufficient. Distinct local tests now cover Responses buffered,
  legacy stream, normalized stream, Chat buffered, Chat stream, and unsupported
  Responses followed by a failing Chat fallback. The terminal fallback asserts
  the Chat response request identifier rather than a stale Responses value.
- 2026-09-08: Corrected the fallback fixture to use `gpt-5.6`, whose Responses
  state is `Allowed`; `gpt-5` requires the Responses API and deliberately
  cannot take the optional Chat Completions fallback. The fixture asserts both
  the allowed state and the API-key backend fallback capability before mounting
  the Responses and Chat mocks.
- 2026-09-19: Began the bounded P1 CodeScene follow-up. The header-metadata
  fixtures now live in a sibling OpenAI test module, and one explicit mode
  drives both legacy and normalized stream entrypoints. No validation has run
  for this follow-up; the serial gate runner remains the exclusive verifier.

- 2026-09-19: C1 partitioned the extracted custom-provider suite into focused
  authentication, error-metadata, sampling, and streaming modules. Named
  `rstest` cases preserve both OpenAI Chat buffered and streaming 429 calls;
  the analogous OpenAI Responses paths use named legacy and normalized cases.
  No validation was run; the serial gate runner remains the exclusive verifier.

## Decisions and risks

- Retain the explicit five-argument retry helper from the preceding plateau:
  policy, error, attempt, mutable backoff, and injected time are independent;
  a wrapper would obscure the deterministic time seam merely to satisfy a
  metric.
- Keep the rate-header module private. If extraction requires a public type or
  an unrelated provider path, stop and obtain a new scope decision.
- Do not hand-wave Anthropic execution coverage. Inspect a real native path; if
  absent, report the exact missing owner before changing additional files.
- The custom-provider suite follows the repository's 500-line Rust warning
  threshold. The four domain modules are smaller through cohesive ownership,
  not an invented 400-line rule.

## Outcomes and retrospective

Pending deterministic and review evidence.
