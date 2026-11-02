# ACP retry observation boundary

## Purpose

This R1 milestone closes CodeRabbit findings `3952440911` and `3952440915`,
plus the retry portion of PR #103's Unit Architecture check. An ACP retry
decision must interpret an HTTP-date against the moment the provider error was
observed. The ordinary public retry methods remain available for existing
callers, while ACP uses explicit observation times.

## Status

**COMPLETE** — implementation was authorized by the ACP hardening programme and
root supervisory direction. The isolated branch was
`review/acp-pr103-r1-retry-boundary-20260919`, based on
`23f2f38ba0bc81cd8bd30f275bab6c02500eba2c`. Initial deterministic validation
passed before commit `3886a272`. The milestone and its review repairs landed on
the PR #103 rate-limit branch, where the shared serial gate runner passed
format, lint, typecheck, and test gates.

## Conformance basis

- PR #103 review intake: CodeRabbit `3952440911` requires a narrow jitter
  lint expectation; `3952440915` requires sibling retry test modules.
- PR #103 pre-merge Unit Architecture finding requires explicit observation
  time at retry boundaries without a universal clock trait.
- Root approved compatibility-first policy: preserve externally callable
  convenience methods and add explicit-time methods for ACP.
- Root clarified that fixed-time regressions must run the actual buffered and
  streaming ACP retry paths, including a headerless follow-up.

## Constraints

The change must preserve retry arithmetic, retry budgets, wire formats, and the
`RetryPolicyCoreExt` convenience API. It must not add a public clock trait,
dependencies, a provider refactor, PR #104 decoder work, or A1/A2 changes. The
only scope extension beyond the earlier packet is `handlers.rs` and its
existing timing test module, approved to test actual ACP call paths.

## Design and invariants

`RetryPolicyCoreExt` gains additive explicit-time methods. Its existing methods
remain compatibility wrappers that call `SystemTime::now()`. ACP captures
`SystemTime` when each provider error reaches its retry decision and passes
that value to the explicit-time method.

The default explicit-time methods keep downstream trait implementations source
compatible by delegating to their existing methods and therefore cannot impose
time injection on them. `RetryPolicy` overrides those defaults and honours the
supplied observation time, which is the implementation ACP uses.

The retry-after conversion helper receives its observation time from its caller.
`From<LLMError> for VtCodeError` remains a compatibility conversion boundary
and captures the current time once before calling a private explicit-time
conversion helper.

The following invariants must hold:

1. A valid HTTP-date produces a relative delay using the supplied observation
   time only.
2. An HTTP-date floor applies to its current decision and is not retained when
   the next error has no header.
3. Integer, fractional, and overflow Retry-After values remain capped at
   twenty-four hours.
4. The existing jitter calculation has identical arithmetic and only replaces
   its over-broad lint suppression.
5. Moving tests preserves private-module visibility and test behaviour.

## Milestone 1: Write the red behavioural coverage

Add fixed-observation ACP fixtures to
`crates/codegen/vtcode-acp/src/zed/agent/rate_limit_timing_tests.rs`. One
fixture uses `generate_with_retry_with_observer` through the buffered path; the
other runs the real streaming prompt loop. Each emits an HTTP-date rate-limit
error, then a headerless rate-limit error, then succeeds. With a fixed
observation time, the first retry waits for the date and the second uses only
local backoff.

Before production changes, the focused tests should fail because the current
production paths obtain wall-clock time internally. This Red result was not run
locally because the serial runner exclusively owns validation.

## Milestone 2: Make observation time explicit

In `crates/codegen/vtcode-core/src/retry.rs`, add explicit-time retry decision
methods and retain legacy wrappers. In
`crates/codegen/vtcode-core/src/retry_after.rs` and
`crates/codegen/vtcode-core/src/error.rs`, pass `SystemTime` explicitly at the
conversion boundary while preserving `From<LLMError>`.

In `crates/codegen/vtcode-acp/src/zed/agent/handlers.rs`, introduce only a
private prompt-local observation callback. Production supplies
`SystemTime::now`; timing tests supply a fixed function. Route the buffered and
all streaming retry decisions through it.

## Milestone 3: Extract tests and narrow lint policy

Move each complete existing `#[cfg(test)] mod tests` block from the commons and
core retry modules into `retry/tests.rs`, leaving `#[cfg(test)] mod tests;` in
the parent. Replace only the jitter cast `allow` with a scoped `expect` that
states the non-negative duration, validated jitter, and clamp invariants.

## Verification plan

The shared serial runner must run the extracted commons and core retry suites,
the retry-after and error suites, and both fixed-observation ACP selectors. It
must then run Rust format checking, changed-crate Clippy with warnings denied,
Markdown and spelling checks for this plan, immutable CodeScene, and the scoped
review flow.

The expected behavioural evidence is that both ACP paths take the HTTP-date
delay relative to the fixed epoch, then the headerless retry uses the
configured local delay rather than retaining the date floor.

## Progress

- 2026-09-19: Created isolated R1 worktree at `23f2f38b`.
- 2026-09-19: Verified the four ACP retry sites and existing paused-clock
  buffered coverage. Root required actual buffered and streaming fixed-time
  regressions, so this plan records the approved private prompt-local seam.
- 2026-09-19: Added additive explicit-time retry methods. The existing public
  trait methods remain required compatibility methods, so external trait
  implementers are not broken; their explicit-time defaults defer to their
  existing implementation.
- 2026-09-19: Routed ACP buffered and all three stream failure paths through a
  prompt-local observation callback. Production supplies `SystemTime::now` at
  each received error; tests supply a fixed epoch.
- 2026-09-19: Added paused-time buffered and full streaming prompt regressions
  that observe an HTTP-date error, then a headerless error, then success.
  Extracted both retry test modules and added a fixed-time conversion test.
  Deterministic gates passed before commit `3886a272`; the serial runner owns
  the review-repair gate rerun.

## Risks and escalation

Stop and escalate if making ACP's time observable requires changing a public
protocol, adding a cross-crate clock abstraction, altering retry arithmetic, or
modifying unlisted PR #104 paths. If the direct streaming prompt fixture cannot
run without a client connection, retain the production seam and seek a bounded
test-harness scope expansion rather than introducing global mutable time.

## Outcomes and retrospective

Implementation is complete. The explicit-time trait methods are defaulted
because a required method on the public extension trait would be a breaking
change for downstream implementers. Verified on head `ca4eff5bf` by
`make check-fmt`, `make lint`, `make typecheck`, and `make test` (10622 passed,
24 skipped), together with the focused ACP rate-limit and retry-boundary
selectors.

## A1 progress

- Extracted wire support and physics tests; added a fixed-epoch ACP transport
  regression.
- Initial serial gates passed: focused ACP tests, strict Clippy, full ACP
  suite, required VidaiMock 0.1.3 physics, and documentation checks.
- Accepted final review refinement: keep the shared `SystemTime` observation
  through all ACP handlers and convert it once inside the private Lody notice
  projection; the fixed-epoch transport regression now supplies
  `UNIX_EPOCH + Duration`.
- Final-nit gates passed before the milestone landed: Rust formatting, the 11
  focused rate-limit selectors, strict ACP Clippy, and plan spelling and
  Markdown checks.

## A2 progress

- Added a feature-gated `TestPerfRecorder` to make awaited telemetry assertions
  possible without changing production recorder behaviour.
- Replaced ACP's raw provider diagnostic projection with a category and optional
  HTTP-status projection for incomplete turns and structured failure logs.
  Provider retry, rate-limit notice, and cancellation paths retain their
  existing runtime behaviour.
- Added marker-based unit and ACP wire coverage that keeps provider diagnostic
  bodies out of client-visible incomplete turns while retaining a classified
  category and status.
- Strengthened `blocked_stop_draft_is_not_visible_until_hook_allows_it` to
  assert the first stop-hook outcome before projection. A recurrence reports
  only bounded, fixed-fixture diagnostics: hook-message levels and truncated
  text, plus the trimmed and truncated `stop-count` fixture value. This does
  not claim that the intermittent full-suite failure is fixed or that the full
  suite has passed.
- The A2 source was frozen for the serial runner's feature-gated core test,
  focused ACP regressions, strict Clippy, and documentation gates; those gates
  passed for the landed candidate.

## A3 finite telemetry progress

- 2026-09-20: A2's module-purpose documentation follow-up passed its scoped
  formatter, spelling, and rustdoc gates and committed as `2a530078`.
- 2026-09-20: Root approved the next bounded implementation plateau: six
  finite ACP provider event contracts through the existing core perf recorder,
  a request-local awaited test sink, and closed diagnostic tags. The packet is
  `/tmp/acp-pr103-a3-finite-telemetry-milestone-packet.md`.
- A3 may not add a global collector, public telemetry trait, `ZedAgent` field,
  protocol field, raw provider-name label, or a second event bus. Buffered and
  streaming handler paths must each be exercised with a distinct awaited test
  recorder before review.
- Implementation now has private finite descriptors and a request-local sink,
  `ProviderRuntimeRegistry` classification, real buffered and streaming retry
  consumers, and bounded rate-limit notice-failure recording. The source was
  frozen for manifest review and exclusive serial validation, which later
  passed for the landed candidate.
- CodeScene review repair keeps the inherited
  `run_prompt_with_retry_observer` identity for the existing high-complexity
  prompt loop. Its reported complexity remains an additive telemetry delta, not
  a claim that a broad loop extraction fixed it. A request-local sink is now an
  explicit fourth argument; the existing fixed-observation contract test
  supplies the same process sink that the former three-argument helper used by
  default.
- The telemetry repair groups failed attempts and rate-limit notifications
  into private named inputs, and records the closed six-event vocabulary via a
  single observation path. The buffered and streaming tests keep named metric
  witnesses for event order, bounded tags, privacy markers, and retry context.
  Exclusive serial validation passed after this repair.

## Rebase onto the Lody usage/activity branch

- 2026-11-02: The rate-limit branch was rebased onto
  `feat/acp-lody-usage-activity-metadata` at `d67379e2f`. That parent had
  already introduced the `AttemptFailureSink` port, the `GenerationFailure`
  input, and the `RateLimitNotice`/`ObservedRateLimitNotice` split, so the
  rebase keeps the parent's port and threads this branch's observation instant
  and metric sink through it rather than restoring the earlier `notice_target`
  tuple.
- `RetryRequest` now carries `failure_sink: Option<&dyn AttemptFailureSink>`
  and is built with `with_failure_sink`; the retry loop announces failures
  through `AttemptFailureSink::announce(AttemptFailure { .. })`. The
  intermediate `RetryFailureNotice`/`publish_retry_notice` pair was dropped
  because the port already owns notice construction.
- The parent's `rate_limit_metadata` shape (outer "is a rate limit", inner
  optional metadata) and its status-aware notice text are preserved, so a rate
  limit without a status still publishes a notice without claiming HTTP 429.
  The parent's `redact_detail` helper is not needed here: this branch's
  operator logs carry only closed categories and attempt counters.
- `record_stream_attempt` keeps the parent's `status` parameter, and
  `publish_lody_rate_limits_at` keeps the parent's metric sink and provider
  class so snapshot delivery failures stay attributed.
- The inherited streaming notice regression asserted the parent's delay
  contract, where a provider `Retry-After` replaced the local backoff. This
  branch treats both the provider hint and the configured initial backoff as
  floors, so the default ten-second backoff dominated the fixture's half-second
  hint and the retry outlived the test's five-second timeout. The regression
  now registers the wire-test provider with a half-second request policy, which
  keeps the scheduled delay equal to the provider hint and preserves the
  assertion that the notice advertises the delay actually scheduled.

## Rebase onto the advanced Lody usage/activity head

- 2026-11-02: The parent branch advanced from `d67379e2f` to `aec962ef0`
  while this layer was being validated, so the layer was replayed again onto
  the new head. The exclusive boundary stayed `d67379e2f`; all 33 layer commits
  were replayed, and the resulting head is `2fb665cb9` plus the post-rebase
  repair `6271743e9`.
- The first replay attempt used the user-wide Weave merge driver. Weave
  truncated `crates/codegen/vtcode-llm/src/providers/openai/provider.rs` to
  four lines while replaying the test-module extraction, which corrupted every
  later commit in the chain. The replay was aborted and restarted with
  `core.attributesFile=/dev/null`, which disables the user-wide
  `*.rs merge=weave` enrolment and uses Git's built-in merge backend with
  `zdiff3`. The built-in backend replayed the same series with only four
  conflicts, all resolved by hand.
- Conflict resolutions:
  - `openai/provider.rs`: the parent moved the inherent `impl OpenAIProvider`
    block; this layer's `provider_error_with_headers` was inserted into the
    parent's block instead of restoring the pre-split file.
  - `scripts/generate_config_field_reference.py`: this layer deletes
    `format_code_span` and inlines simple spans; the parent's backtick-padding
    fix is preserved by this layer's later `code_span` helper, so the deletion
    was kept.
  - `handlers/tests/prompt.rs`: kept the parent's `mod interrupted_tool;`
    alongside this layer's module declarations.
  - `handlers/tests/session.rs`: the parent split the file; this layer's
    `assert_first_stop_hook_block` helper and its call site were applied to the
    parent's smaller file.
  - `scripts/large_file_allowlist.txt`: kept the parent's tighter 1.8 MB
    ceiling and removed the duplicate 2 MB entry in `6271743e9`.
- The parent's `f73ce33ac` made `AttemptFailureSink` cross-task safe
  (`Send + Sync`). This layer's `RateLimitNoticeSink` already meets those
  bounds, so the replayed retry loop announces failures through the hardened
  port without an adapter.
- The parent's `0a8099406` split the oversized ACP handler test modules. The
  replayed `assert_first_stop_hook_block` helper and this layer's
  `rate_limit_notice` module were applied to that split layout rather than
  restoring the pre-split files.
- The parent's `format_code_span` padding fix and its `--locked` schema
  invocation are retained. The generated
  `docs/config/CONFIG_FIELD_REFERENCE.md` was not regenerated: the generator
  still drops the three custom-provider pricing and rate-limit-header rows.
