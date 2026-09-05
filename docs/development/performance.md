# Performance Optimization

VT Code uses a local-first performance workflow. Performance checks are measured manually and are not hard CI gates. The default stance is simple: do not guess, measure first, and only keep complexity that pays for itself.

## Goals

- Keep release artefacts portable.
- Improve runtime without hurting day-to-day iteration speed.
- Optimize only measured hotspots.

## Performance & Simplicity Rules

- Do not guess where time goes. Capture a baseline before changing code that claims a performance win.
- Measure before tuning. Keep before/after numbers from `baseline.sh`, targeted timers, or benchmarks.
- Prefer simple algorithms when input sizes are small or not yet proven large.
- Avoid fancy algorithms and broad refactors unless measurements justify their constant-factor and maintenance cost.
- Start with data structures and layout. In VT Code, the right cache shape, queue boundary, or representation usually matters more than clever control flow.

These rules apply to product code and refactors alike. The burden of proof is on the optimization, not on the simpler baseline.

### Filter before expensive projection or normalization

Establish relevance before performing expensive projection or normalization work. In practice, filter a catalogue, request, or candidate set before serializing schemas, constructing derived views, or normalizing data that will not be used. This keeps common paths from paying for work that only matters after a policy or relevance check succeeds.

The default documentation gate follows the same principle: `cargo doc --workspace --no-deps` builds the public API documentation and intentionally excludes private items. Private-item documentation expands the work to internal implementation details that are not part of the contributor-facing API artefact. Maintainers who need internal API inspection can opt in with `cargo doc --workspace --no-deps --document-private-items`.

### Bounded I/O on the agent hot path

Independent code-search backends (literal, declaration, and path search) are
started together with `tokio::join!`; filesystem reads, tree-sitter parsing,
and candidate aggregation run in one `spawn_blocking` task. Keep the async
coordinator responsible for ordering and cancellation, not synchronous disk
work. For synchronous side-channel APIs such as progress monitoring, use a
bounded coalescing writer so callers replace stale snapshots and never wait on
filesystem latency.

### Agent-loop hot-path invariants

Tool-result cache size is measured from the payload bytes, not the `String`
container. Replacing an existing key updates the byte total in place, so a
full cache does not evict an unrelated entry during replacement; zero-capacity
caches reject inserts. Keep these accounting rules intact when changing cache
entry representations.

Clean request histories are borrowed and shared with continuation state through
`Arc<Vec<Message>>`. Copy only when editor/few-shot context must be injected or
the provider requires compaction. This keeps the common no-injection path from
allocating multiple equivalent histories while preserving the existing
normalization and continuation boundaries.

Request envelopes retain the source tool-catalogue `Arc` within a request segment.
When model/provider/mode/prompt identity is unchanged, subsequent turns reuse
the frozen ordered catalogue without cloning, sorting, or re-hashing its schema;
segment boundaries clear that marker before rebuilding.

Read-only tool calls are batched only after per-call preflight confirms that
each call is parallel-safe; duplicate names are not a safety signal. Batch line
ranges still pass through the absolute read cap, so new range-reading paths
must preserve that limit. Unified and runner dispatch both apply
`agent.harness.max_parallel_tool_calls`; zero is the explicit unlimited value.
Mutating or otherwise non-parallel calls remain ordered.

Legacy text reads use the same bounded line reader as paged reads. This keeps
large files and minified one-line bundles from creating an unbounded temporary
buffer; invalid UTF-8 remains lossily decoded for compatibility. A physical
line that exceeds the bound is reported as `line_truncated` so the agent can
switch to byte ranges or targeted inspection instead of treating the preview
as complete. Live command-output spools are never result-cached, and
directory-scoped read caches invalidate on descendant edits. A command-cache
miss also invalidates filesystem-derived results before shell/PTY execution,
while completed read-only command cache hits remain reusable.

### Serialization and event-log replay

Avoid combining `#[serde(flatten)]` with `#[serde(untagged)]` on frequent,
discriminator-driven protocol payloads. Serde must buffer the surrounding map
to decide which flattened shape applies; direct wire structs can decode the
known fields once and construct the tagged payload afterwards. VT Code uses
this for OpenResponses and ACP streaming notifications. Keep flattening when
it is the actual contract, such as trace metadata's vendor-extension map.

Keep streaming payloads as borrowed SSE text until a consumer needs an owned
payload. The normalized Responses adapter now avoids the old
`Value -> JSON -> Value` round trip; common text, reasoning, tool, and
lifecycle events use typed decoding, with full payload materialization reserved
for completion and compatibility fallbacks.

Session-log index rebuilds have an even narrower requirement: they need the
versioned envelope and `event.type`, not the full event payload. The rebuild
path therefore skips nested payload materialization, while turn reconstruction
continues to use the canonical `VersionedThreadEvent` decoder.

### Privacy-preserving harness trace analysis

Use `vtcode_eval::analyse_jsonl_file` or `analyse_jsonl_reader` for offline
DeepSeek/VT Code harness analysis. The file and reader paths process one JSONL
record at a time (with a 1 MiB record limit), retain only aggregate counters, and never copy prompts,
arguments, paths, file contents, output text, or free-form error messages into
the returned summary. Tool names and error values are mapped to bounded known
labels; unknown values are grouped under `other_tool` or `error`.

The analyser treats `ThreadCompleted` usage as a fallback when no per-turn
usage exists, so a normal thread trace does not double-count its aggregate.
Latency count, total, and maximum cover the complete trace; percentile queries
use a bounded 4,096-sample reservoir to keep memory usage stable for large
sessions. Use the summary to compare tool repetition, error categories, token
cache usage, and output volume before changing the runtime.

## Local Workflow

```bash
# 1) Capture baseline
./scripts/perf/baseline.sh baseline

# 2) Make a targeted change

# 3) Capture latest
./scripts/perf/baseline.sh latest

# 4) Compare results
./scripts/perf/compare.sh
```

Artefacts are written to `.vtcode/perf/` and include JSON metrics plus raw logs.

The perf harness builds and measures `target/release/vtcode`, not `cargo run`
or the debug binary. It clears `RUSTC_WRAPPER` and
`CARGO_BUILD_RUSTC_WRAPPER` by default for its cargo steps so local
measurements still work when `sccache` is configured but unavailable. Set
`PERF_KEEP_RUSTC_WRAPPER=1` only when you explicitly want to keep the wrapper.

Use this loop for any non-trivial performance change. Change one thing at a time so the comparison stays attributable.

## Standalone startup benchmark

Startup policy is defined by the command case and launch state, not by one
generic `startup_ms` number. Measure the release executable directly:

```bash
cargo build --release --locked --bin vtcode
VTCODE_BIN="$PWD/target/release/vtcode" \
  VTCODE_BENCH_RUNS=10 \
  cargo bench --locked --bench startup -- --noplot
```

The standalone case matrix is:

```text
vtcode --version
vtcode --help
vtcode schema tools --format ndjson --name code_search
```

Run every case as both a cold and warm sample. Cold means copying the
executable to a new temporary path for each launch, then timing the launch of
that freshly copied executable;
this approximates fresh executable loader/relocation work. Warm means timing
repeated launches of the same executable after warm-up. Cold does not mean
flushing the operating system's page cache: the harness never flushes or
evicts OS page caches, so label and compare the result as a fresh-copy cold
proxy.

Every child receives an isolated temporary `HOME`, config root, data root,
explicit config-file path, and workspace. This prevents credentials, user
configuration, persistent data, and repository contents from affecting the
startup result. The schema case is still useful because it exercises tool
registry construction while remaining standalone and provider-free.

For each case and launch mode, retain the raw millisecond samples and report
the median and p95. The median is the primary central result; p95 exposes
startup tail behaviour. Use the same binary, machine, environment, sample
count, and isolation layout for before/after comparisons. Keep
`VTCODE_STARTUP_TRACE=0` (or unset) during timed runs; enable
`VTCODE_STARTUP_TRACE=1` only for a separate diagnostic run.

The broader capture remains available when its additional workloads are
needed:

```bash
./scripts/perf/baseline.sh baseline
./scripts/perf/baseline.sh latest
./scripts/perf/compare.sh \
  .vtcode/perf/baseline.json .vtcode/perf/latest.json
```

It records separate cold fresh-copy, warm, first-user-I/O, and interactive
first-render artefacts. Those metrics must not be treated as substitutes for
the three-case standalone matrix above.

For phase-level diagnostics, set the opt-in trace before launching the binary:

```bash
VTCODE_STARTUP_TRACE=1 target/release/vtcode --provider ollama --model llama3
```

The trace is silent when unset and reports only duration records for bootstrap,
CLI parsing, runtime creation, config, validation, authentication, session
setup, and first UI render. It is initialized before tracing is configured so
early startup work is observable without adding work to normal launches.

### Patterns that pay off on the startup path

- **Join independent disk I/O.** `initialize_dot_folder`, `init_global_guardian`,
  `determine_theme`, and `resolve_runtime_provider_auth` only depend on config
  that is already resolved; run them through `tokio::join!` so their disk reads
  overlap instead of running serially.
- **Gate inits behind `command_skips_provider_auth`.** Commands that never run
  tools (Login, Logout, Auth, ToolPolicy, AppServer, Notify, Pods, Schedule)
  do not need the guardian, file/command caches, gatekeeper, session-archive,
  or perf-telemetry init — skip them entirely.
- **Keep file reads bounded.** The dotfile audit log (`audit.rs::read_last_hash`)
  is append-only and grows unbounded; read only the tail window so startup cost
  stays `O(window)`, not `O(file size)`.
- **Defer non-critical background work.** Temp-spool cleanup
  (`cleanup_old_temp_spools`) runs in `spawn_blocking` so a cold user-cache
  `large-output/` directory
  never blocks first user I/O.

### Release artefact assumptions

The shipped `release` profile remains tuned for launch size and dead-code
removal: `opt-level = "z"`, full LTO, `codegen-units = 1`, stripping, and an
abort-on-panic runtime. macOS release scripts and `.cargo/config.toml` also
apply `-Wl,-dead_strip`. Verify the effective profile and the measured binary
size before attributing a result to Rust startup code; a debug binary is not a
valid proxy for the shipped launch path.

Cold and warm results answer different questions. Warm results isolate process
and loader overhead after the binary is resident. Fresh-copy results expose
the size and relocation cost paid by a newly spawned process, which is the
relevant signal for subprocess-heavy workflows. Interactive results additionally
include configuration, authentication, terminal initialization, and session
setup through the first usable frame.

The default binary links heavy subsystems that most invocations never use:

- `vtcode-eval` — eval framework (only `vtcode eval` commands).
- `vtcode-acp` — Agent Client Protocol (only `vtcode acp`).
- transitively via `vtcode-core`: `vtcode-indexer`, `vtcode-mcp`, `vtcode-a2a`,
  `vtcode-skills`.

These are potential binary-size levers. Cutting them requires feature-gating
them out of the default binary (and behind an opt-in feature for the commands
that need them). That is a product decision, so it is intentionally not done
silently. Measure cold-start impact with:

```bash
./scripts/perf/baseline.sh latest
```

## Profiling Build

Use this when collecting profiler traces:

```bash
./scripts/perf/profile.sh
```

This builds release with:

- `-C force-frame-pointers=yes`
- `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`

Then profile `target/release/vtcode` with your preferred tool.

## Local Native Tuning

For local experiments only:

```bash
./scripts/perf/native-build.sh
./scripts/perf/native-run.sh -- --version
```

These scripts append `-C target-cpu=native` for local runs only. They do not change portable release defaults.

## Benchmarks

Current Criterion benches:

```bash
cargo bench -p vtcode-core --bench tool_pipeline
cargo bench -p vtcode-core --bench agent_harness
```

Use benches when a hotspot is stable and repeatable. Use the baseline/profile scripts when the question is broader end-to-end behaviour.

### Interactive latency workloads

The `agent_harness` target measures the repeated work that affects interactive
requests: warm prompt-resource cache hits, few-shot tag selection, tool
definition sorting during catalogue refresh, scoring against a warm indexed file
list, and cold-cache tool-catalogue assembly under hosted, client-local, and
disabled deferred-loading policies. Its fixtures are deterministic and keep
filesystem setup outside the timed iterations.

Prompt resources use canonical source paths, a five-minute bounded cache, and a
two-second metadata polling interval. Cache misses perform scans, reads, and
parsing on Tokio's blocking pool; warm prompt assembly does not reread or
reparse unchanged resources. Indexed searches read an immutable path-text table
by `StringId`; incremental updates publish a new table so searches holding an
older index remain safe.

Workspace tool registration also retains the effective persistent-memory
configuration from its startup parse. Memory tool calls no longer reload and
reparse `vtcode.toml`; this follows the same snapshot policy as the web-tool
and output-spooler settings while preserving the disabled-memory guard.

Basic directory-list cache keys include the canonical workspace and every
response-shaping filter and pagination value. A cached listing therefore stays
local to its workspace and cannot satisfy a request with a different list
shape; the async path reuses its single metadata result for directory checks.

The same target also includes uncached filesystem workloads:
`agent_harness_file_search_uncached` measures parallel traversal and bounded
candidate aggregation, while `agent_harness_file_index_build` measures a full
index construction per iteration. `agent_harness_tool_catalog_projection_repeat`
measures repeated schema/model-tool projection after the catalogue is warm; its
projection cache is private to an immutable catalogue and keyed by documentation
mode. These benchmarks expose repeated work and synchronization cost rather
than serving as universal CI thresholds.

Code-search changes should be checked for both backend overlap and blocking
pool behaviour; progress-ledger changes should be checked for bounded queue
growth and latest-snapshot semantics before comparing end-to-end medians.

Compare repeated local medians rather than adding a noisy hard gate:

```bash
./scripts/perf/baseline.sh baseline
./scripts/perf/baseline.sh latest
./scripts/perf/compare.sh
```

Rustc-specific AST shrinking, compiler incremental-cache changes, and PGO are
outside this runtime-focused wave. Revisit them only with a confirmed VT Code
profile hotspot and a separate build-performance budget.

## Optimization Rules

- Change one thing at a time.
- Keep changes surgical and behaviour-preserving.
- Prefer simple, safe single-pass reductions over broad refactors.
- Revisit data structures before introducing algorithmic sophistication.
- Keep the simplest implementation until measured workload data proves it insufficient.
- For hashers, follow the selective policy in `performance-hasher-policy.md`.
