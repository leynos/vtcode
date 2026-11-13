# Local Performance Scripts

These scripts provide a repeatable local performance workflow for Diapason.

## Commands

```bash
# Capture metrics + raw logs
./scripts/perf/baseline.sh baseline
./scripts/perf/baseline.sh latest

# Compare two captured runs
./scripts/perf/compare.sh \
  .diapason/perf/baseline.json \
  .diapason/perf/latest.json

# Build release binary for profiling (line tables + frame pointers)
./scripts/perf/profile.sh

# Local-only host-tuned build/run
./scripts/perf/native-build.sh
./scripts/perf/native-run.sh -- --version
```

The baseline builds and measures `target/release/diapason`. It captures release
binary size, cold launch from fresh `/tmp` copies, warm `--version`, a
credential-free `tool-policy status`, and interactive first-render latency
through a PTY that answers terminal capability queries. It also retains the
`diapason-core` pipeline and harness benchmarks for local comparison; none of
these measurements is a CI performance gate.

Criterion workloads are skipped by default so launch measurements do not wait
for a full benchmark-profile rebuild. Set `PERF_RUN_BENCHMARKS=1` to include
them in the JSON capture.

## Outputs

All artefacts are written to `.diapason/perf/`:

- `baseline.json` / `latest.json`: captured metrics
- `*-cargo_check.log`: cargo check output
- `*-bench_tool_pipeline.log`: `diapason-core` tool-pipeline bench output
- `*-bench_agent_harness.log`: `diapason-core` interactive harness and
  optimization bench output
- `*-cold_startup.json`, `*-warm_startup.json`, `*-first_user_io.json`: raw
  release launch samples
- `*-interactive_first_render.json`: raw PTY prompt-render samples
- `diff.md`: markdown comparison report

## Notes

- Cargo steps retain the managed `RUSTC_WRAPPER` so builds participate in
  shared compile admission and cache accounting.
- `startup_ms` is retained as an alias for `warm_startup_ms` for compatibility
  with older reports.
- `cold_startup_ms` measures three launches of fresh copies in `/tmp`; it is a
  fresh-copy loader/process signal, not a page-cache eviction benchmark.
- `interactive_first_render_ms` ends when the PTY sees the initial
  `Type a request` prompt and terminates the isolated sample.
- The harness uses a release build and a credential-free temporary `HOME`
  /config directory, so it does not use provider credentials or write to the
  real user config.
