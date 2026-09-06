# Debugging plan: hook-chain stdin race

Generated: 2026-09-05. Related PRs: #20 and #23.
Falsification sub-agent: `alchemist`.
The planning agent prepared this document; the sub-agent executes the experiment.

## Observation

The PR #23 full suite lost the first hook's `rtk` rewrite but retained the
second hook's `--audited` suffix. The same test passed on both lower layers.
The test and execution path are unchanged from the train's initial base.
The first shell hook prints JSON without reading stdin. The engine writes its
payload before collecting stdout; a write error skips interpretation.
The failure log contains no hook messages, so its exact cause is unobserved.

## H1: early child exit prevents interpretation

Claim: the first hook can exit before the parent's stdin write, causing a
BrokenPipe error that discards the first rewrite. Plausibility: high.
Prediction: forcing this legal process ordering produces the observed suffix
without the prefix, and reports a payload-write error.

## Falsification experiment

Use only `test_pre_tool_use_hook_chain_rewrites_feed_later_hooks` in
`vtcode-core`, with the shared Cargo cache and existing target directory.
The gate runner is paused. Capture commands and output with `tee` under `/tmp`.

1. Save exact original bytes of `engine.rs` and `tests/hook_tooling.rs` under
   the lifecycle directory to an external scratch directory.
2. Temporarily add the outcome messages to the final assertion's diagnostic.
   In `execute_command`, only for the first command in this named test
   (the literal JSON command containing `rtk cargo build`), wait for that
   child to exit after taking its stdin handle and before writing stdin. This
   forces the suspected ordering without sleeps or external load. Do not
   apply the wait to the second Python hook or any other test.
3. Run the single named test once. H1 is falsified if this forced ordering
   still preserves the prefix, or loses it without the predicted write error.
4. Restore the engine exactly. Change only the first test command to consume
   stdin before emitting its unchanged JSON, then run the same test once.
   If that still loses the prefix with no command error, H1 is insufficient.
5. Restore both source files byte for byte regardless of outcome. Report the
   exact commands, logs, diagnostics and hashes in an external report.

Do not run full gates, format, commit, push, change branches, or edit the
staged handoff. Do not introduce additional hypotheses or production fixes.
Stop after these two valid executions. If compilation prevents execution, report
inconclusive rather than broadening the experiment.

## Interpretation boundary

A surviving hypothesis demonstrates the mechanism, not the historical
failure's unrecorded error. A fixture repair belongs in #20. Any inherited
production handling defect requires a separately scoped issue and must not
ride in a mechanical spelling PR. Keep the chain's exact assertion intact.

## Experiment correction

The first attempt waited before taking the child's stdin handle. Tokio's
`Child::wait` takes and drops that handle itself, so the engine skipped its
write block. The passing result is inconclusive. For the corrected attempt,
retain the handle in the existing `if let Some(mut stdin)` block, wait for
the selected child there, then write through the retained handle. This
preserves the operation being tested. One corrected forced-order execution
is authorized; do not repeat the drain-input control if already completed.

## Results

The corrected forced-order test failed with the exact original difference:
`cargo build --audited` instead of `rtk cargo build --audited`. Its diagnostic
reported `failed to write lifecycle hook payload`. The control drained stdin
before emitting the same JSON; the unchanged chain assertion passed.

H1 was not falsified. This reproduces a sufficient mechanism without claiming
to recover the historical failure's unrecorded error. The runtime follow-up
is tracked in [issue #54](https://github.com/leynos/vtcode/issues/54).
The PR #20 fixture repair follows the guide's existing stdin-draining contract
and adds hook messages to the assertion diagnostic. It changes no production
handling and does not close issue #54.

Both focused runs used `cargo nextest run --locked -p vtcode-core` with the
single named test filter. The forced-order run failed one test; the control
passed one test. Each skipped 3,560 other tests. Logs are under `/tmp`, with
these action prefixes and the worktree and branch suffixes:

- `test-hook-stdin-forced-exit-corrected`
- `test-hook-stdin-consuming-fixture-corrected`

The temporary production instrumentation was restored byte for byte before
applying the fixture repair and running the full commit gates on PR #20.
