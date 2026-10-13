# PR #56 review convergence

## Scope and decision

Repair only the two valid Codex findings on PR #56 head
`b928ed34deded93fded5ff9be3397d8dffbcaf2a`, based on PR #55 head
`1321e0de6d92b5da997be6245d2430f53225b028`.

- `r3952424881`: bound live-probe response capture and diagnostics to 16 KiB,
  retain the exact prefix, and record truncation truthfully.
- `r3952424885`: reject missing or non-array terminal `output` as a decode
  failure rather than a schema-valid `NoCall`.

The work owns only `friendli_probe.rs`, its offline replay tests, and the live
capture metadata. It makes no paid Friendli request, provider behaviour change,
branch topology change, publication, or review request.

## Progress

- [x] Verify both Codex findings against the exact PR delta and module contract.
- [x] Add bounded capture and truncation classification with offline coverage;
  shared-runner execution remains pending.
- [x] Require an array terminal output with malformed-shape coverage; shared
  runner execution remains pending.
- [ ] Run the shared scrutineer packet: focused replay tests, formatting,
  relevant locked check/lint, then the applicable full PR gates.
- [ ] Publish and reply to both threads only after gates verify the committed
  repair; request CodeRabbit only after deterministic checks pass.

## Decision log

`NoCall` remains valid only for a completed response with a schema-valid output
array containing no call. Capture overflow is a local incomplete decode: it is
reported as `DecodeFailure`, never as a successful or replayable provider result.
The raw capture is a prefix capped at 16 KiB, and `response-meta.json` records
the truncation flag so its byte contents are not misrepresented as complete.

## Verification plan

The offline `friendli_probe_replay` target must prove that an oversized local
HTTP response leaves a 16 KiB prefix, sets the flag, and fails classification;
it must also prove missing and non-array output values cannot become `NoCall`.
No gate, formatter, lint, build, or review runs outside the shared scrutineer.
