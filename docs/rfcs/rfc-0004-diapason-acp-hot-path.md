# RFC 0004: Rightsize the ACP event-loop hot path

- Status: Proposed; documentation only, with no measured speed-up claimed.
- Date: 2026-11-07.
- Codename: `shoopdawoop`.
- Scope: ACP scheduling, initialization, forwarding, and bounded resource use.
- Delivery: [ACP hot-path roadmap](../project/diapason-acp-hot-path-roadmap.md).

## 1. Problem and intended outcome

Many independent Diapason agents share a host with compilation, orchestration,
and tool subprocesses. A runtime that is reasonable alone may multiply thread,
queue, initialization, and retention costs at that deployment density.

Optimize the production Agent Client Protocol (ACP) path for predictable
latency, cancellation, and bounded resources at 1, 10, and 50 agent processes.
The primary goal is useful work per host, not minimum thread count or a faster
synthetic loop at the expense of correctness. Separate protocol readiness,
session initialization, provider waiting, and event forwarding.

This proposal does not rewrite ACP, introduce a second agent loop, weaken
security, replace Tokio, or implement a shared daemon. It does not remove
features covered by the separate `kthxbai` proposal or change the allocator
covered by `ytmnd`. Those changes must not contaminate paired measurements.

## 2. Evidence and hypotheses

The inspected baseline is `0c54619cb827e48bf09ea3b68b41cd6d2e72d905`. Bootstrap
constructs a Tokio multi-thread runtime without an explicit worker count and
uses a dedicated main thread with a 16 MiB stack reservation.[^1] Tokio
supports configurable worker counts; its blocking pool is separate.[^2] A
reservation is not proof that all of those bytes are resident. Actual host CPU
visibility, environment, thread counts, and proportional set size (PSS) must be
measured.

ACP eagerly builds its launch-workspace runtime, including a core tool registry
and optional subagent/MCP setup. Registry construction iterates the built-in
tool packs before ACP projects its model-facing catalogue.[^3] MCP discovery
currently precedes the catalogue snapshot, so deferring it without a
replacement contract can silently remove tools.[^4]

The existing standalone startup benchmark measures version, help, and schema
commands. It is not an ACP readiness or long-running event-loop benchmark.[^5]
The performance guide already describes typed stream decoding, bounded file
reads, and projection caches. Profile the integrated implementation rather than
claim those existing improvements as new work.[^6]

The hypotheses are that a smaller worker budget can improve host density, that
selecting required capabilities before constructing them can reduce startup
work, and that measured queue/lock/allocation hotspots can improve forwarding
latency. No particular queue, cache, or lock is declared defective without
traces from its actual production path.

## 3. Invariants that optimization cannot trade away

Retain the existing `ThreadEvent` vocabulary and official ACP adapter. Keep
per-session turn admission, steering order, cancellation, exact tool identity,
workspace isolation, approval, retry-after floors, provider admission, and
partial-stream recovery semantics. Do not acquire a second provider permit in
the ACP layer or claim that a per-process limit coordinates 50 processes.[^4]

Integrate correctness work on steering, commentary, protected-task compaction,
and interrupted-tool recovery at the chosen implementation base. Record which
fixes actually landed. Do not infer integration from an open PR or its issue
text, and do not postpone independent correctness repairs for this programme.

Never acknowledge completion earlier, omit required durable writes, discard
user instructions, or suppress usage/error events to improve a benchmark.
Queues and caches remain workspace/session/configuration scoped. A change in
credentials, tool catalogue, or policy must invalidate the relevant derived
state before it can authorize or describe a new operation.

## 4. Runtime and initialization design

### 4.1. Right-sized, host-owned execution budgets

Compare the current multi-thread default with one and two multi-thread workers.
One multi-thread worker is not a current-thread runtime. Do not switch to
current-thread execution without a separate compatibility justification,
including any `block_in_place` use and local-task requirements.

Preserve `TOKIO_WORKER_THREADS` support or document an explicit precedence
rule. Resolve the effective budget before runtime construction through trusted
launcher/configuration inputs; a repository-local file cannot enlarge host
resource limits. Report the effective value, not just the requested setting.
Avoid loading the full configuration twice to configure the runtime.

Audit the 16 MiB main-thread reservation and worker stacks, but change stack
sizes only with evidence from deep inputs, large futures, and supported target
checks. Virtual address-space reduction is not a physical-memory result.

Bound admission for CPU-heavy and filesystem work before it reaches blocking
execution. A blocking-thread ceiling alone does not bound the pending queue.
Use workload-specific concurrency limits, cancellation-aware admission, and
bounded inputs; avoid one blocking job per token. Started `spawn_blocking` work
cannot generally be aborted, so scanners need cooperative cancellation or
bounded work units. Tool subprocess termination retains its existing
kill-and-reap semantics.[^2]

### 4.2. Initialize only what the accepted capability set needs

Distinguish process bootstrap, workspace setup, session setup, first prompt,
and first use. Measure each boundary separately. Prefer cheap immutable tool
metadata plus lazy heavy handlers where that preserves the same visible tool
catalogue and permission checks.

Move effective profile selection before expensive pack construction, but do not
equate "not currently exposed" with "never needed": tool discovery and later
explicit selection must still work. Reuse scoped workspace/runtime ownership
and immutable projections. Do not share mutable permissions, credentials,
session histories, or MCP clients across unrelated workspaces or processes as a
shortcut.

Defer an MCP connection only when a trustworthy, versioned descriptor can
preserve discovery, authorization, and invalidation. A cached schema does not
authorize launching a command. Preserve provenance-bound approval and sandbox
requirements, including the concern tracked by issue #187.[^7] If discovery
requires connection or no safe invalidation contract exists, retain bounded
eager setup and report its cost. Do not turn this into a new MCP registry or
sharing daemon project. Optional setup may run concurrently only where its
ownership and dependencies permit it.

### 4.3. Bound forwarding without corrupting streams

Profile the actual receive, normalize, project, enqueue, and write boundaries.
Record queue wait and lock-held duration, payload bytes, wakeups, and work per
provider chunk. Move synchronous filesystem, expensive parsing, and large
serialization out of cooperative polling only when evidence identifies them.
Avoid holding shared locks over network, disk, or client-write waits.

Define both item and byte budgets for application-owned queues, including
pending steering, tool progress, and client output. Count retained in-flight
payloads as well as queued messages; a channel capacity alone is not a byte
bound. Limits must reject oversize input or surface overload explicitly rather
than turn it into success or silently discard it. Do not use spawn-per-message
tasks to evade the bound.

Lossless text/reasoning deltas, tool arguments, approvals, usage, errors, and
terminal outcomes must retain order and content. Coalescing is allowed only for
explicitly replaceable progress snapshots with the same identity and no
intervening lifecycle boundary. Any text-chunk batching must preserve exact
content and order, impose a byte limit and maximum flush delay, and keep
first-output latency within the declared budget. Measure before adding it.

Cancellation and local shutdown must remain actionable when output blocks. Do
not require an acknowledgement of cancellation to traverse a saturated ordinary
data queue before cancellation takes effect. A wedged pipe may still prevent
delivery to the client: after a bounded write deadline, report a transport
failure through the supported lifecycle and terminate/clean up as specified.
Never promise delivery over an unwritable transport or reorder ACP messages
around protocol dependencies.

Own spawned tasks and shutdown explicitly. Stop admission, cancel owned work,
finish required checkpoint boundaries, and join within a documented budget.
Treat non-cancellable blocking work and orphaned children as explicit failures,
not detached successes.

## 5. Measurement contract and gates

Use a small Python driver around the real binary and existing local provider,
ACP duplex, and tool fixtures. No paid inference, live credentials, new runtime
framework, or always-on profiler is required. Reuse existing tests and
observability seams; export is optional and sanitized.[^8]

Measure monotonic boundaries: process launch to ACP initialize response;
`session/new` request to response; prompt admission to first provider request;
provider chunk received to client-visible update; tool request to admitted
execution; cancellation request to local cancellation and terminal outcome.
Report provider-controlled delay separately from local overhead. A faster
handshake that merely moves unchanged work into the first prompt is not an
end-to-end win.

Use fixed fixtures for short streaming, large tool output, sustained multi-turn
history, concurrent sessions, slow and stopped readers, slow MCP setup,
provider backoff, and cancellation during tool/disk work. Validate steering and
failure traces as well as the successful path. At 1, 10, and 50 processes,
record threads, CPU time, context switches, event-loop delay, queue item/byte
high-water marks, PSS/private memory, and process-tree/cgroup memory. Separate
agent, child-tool, MCP, and compilation costs.

Start at low density and increase only under a declared memory/CPU ceiling. An
environment unable to run 50 agents reports that limit; it must not invent an
extrapolated result. Preserve compiler caches and host build admission.

Record commit, lockfile, toolchain, target, profile, allocator, worker budget,
fixture digest, CPU affinity/quota, and background load for every arm of a
paired run. Hold that full list constant across each comparison, except for the
single variable the comparison tests: let the commit differ for candidate
versus baseline and the effective worker budget differ for the experiment
against the default in section 4.1. Never vary more than the one tested
variable, and record both arms' values rather than only the shared ones. Use at
least five fresh-process paired repetitions, alternating order. Treat the run,
not every token or sibling process, as the independent experimental unit.
Retain raw samples and uncertainty alongside medians and p95 values.

Proposed merge gates, to ratify before measurement, are unchanged semantic
traces, enforced queue/shutdown bounds, and a repeatable material improvement
in the targeted bottleneck. Use 15% improvement as the initial materiality
threshold and 5% as the maximum p95 latency/throughput regression in unaffected
workloads. Record absolute differences and measurement noise; an inconclusive
result does not justify changing defaults. A boundedness/correctness fix can
land for that reason without pretending to be a speed-up. Declare and review
any changed threshold before collecting the deciding evidence.

## 6. Sequencing and alternatives

The roadmap first captures representative evidence, then rightsizes scheduling
and initialization, then changes only demonstrated hot paths. Each candidate
must name its falsifier and rollback before implementation. Keep allocator,
compiler optimization-profile, and TUI-removal changes out of paired patches.
Rebaseline after those independent changes land.

Use the #39 application-boundary programme where a real seam is required; full
completion of #50 is not a prerequisite for worker-count experiments or bounded
queue fixes.[^9] Do not rework already optimized SSE decoding on speculation.
Do not disable durable state, security, useful reasoning updates, or telemetry
semantics to reduce overhead. Broader cross-process sharing and scheduler
replacement require separate evidence and proposals.

## 7. References

[^1]: [Bootstrap and runtime construction](../../src/main.rs).
[^2]: Tokio's
      [runtime builder](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html)
    and [blocking work contract](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
    Verify behaviour against the locked version during implementation.
[^3]: [Workspace setup](../../crates/codegen/diapason-acp/src/zed/agent/session_workspace.rs),
    [registry builder](../../crates/codegen/diapason-core/src/tools/registry/builder.rs),
    and [pack registration](../../crates/codegen/diapason-core/src/tools/registry/pack.rs).
[^4]: [Optional ACP setup](../../crates/codegen/diapason-acp/src/zed/agent/optional_setup.rs)
    and [ACP runtime contracts](../development/acp-runtime.md).
[^5]: [Standalone startup benchmark](../../benches/startup.rs).
[^6]: [Existing performance work](../development/performance.md).
[^7]: [Workspace MCP launch approval #187](https://github.com/leynos/vtcode/issues/187).
[^8]: [Observability RFC](rfc-0002-diapason-observability.md).
[^9]: [Remediation programme #39](https://github.com/leynos/vtcode/issues/39)
    and [shared turn execution #50](https://github.com/leynos/vtcode/issues/50).
