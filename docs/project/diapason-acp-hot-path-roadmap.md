# ACP event-loop hot-path roadmap

This roadmap delivers [RFC 0004](../rfcs/rfc-0004-diapason-acp-hot-path.md),
codename `shoopdawoop`. Status: proposed; no optimization or benchmark result
ships with this plan. Phases are falsifiable ideas, steps are workstreams, and
numbered tasks are review-sized changes with measurable outcomes.

## 1. Establish representative evidence and budgets

Idea: real ACP process measurements distinguish scheduling overhead from
provider delay, compilation, tool children, and already-skipped TUI work.

### 1.1. Freeze contracts and experimental controls

- [ ] 1.1.1. Pin the implementation and correctness baseline.
  - Record which steering, commentary, compaction, and interrupted-tool fixes
    actually exist at the selected commit; preserve their regression fixtures.
  - Success: an inventory distinguishes existing failures from regressions and
    names protected protocol, durability, permission, and provider contracts.
  - See [RFC sections 2-3](../rfcs/rfc-0004-diapason-acp-hot-path.md).

- [ ] 1.1.2. Ratify measurement boundaries, budgets, and stop conditions.
  - Define readiness, first useful output, forwarding, cancellation, byte/item
    queue limits, shutdown, host-load ceilings, and performance materiality.
  - Success: a preregistered experiment record fixes controls, repetition,
    thresholds, and exclusions before deciding measurements begin.
  - Keep allocator and build-profile changes outside each comparison.
  - Requires 1.1.1.

### 1.2. Measure the production path

- [ ] 1.2.1. Add a bounded offline ACP benchmark driver.
  - Reuse the real binary and local provider/tool/duplex fixtures; capture raw
    timing, resource, and semantic-trace evidence without live credentials.
  - Success: 1-, 10-, and resource-permitting 50-process cases reproduce the
    RFC workload matrix; slow-reader and cancellation cases cannot hang it.
  - Requires 1.1.2; see
    [RFC section 5](../rfcs/rfc-0004-diapason-acp-hot-path.md).

- [ ] 1.2.2. Rank measured bottlenecks and select the smallest candidates.
  - Attribute waits to scheduling, initialization, provider, locks, projection,
    output, or children; identify existing optimizations before proposing more.
  - Success: each selected change has a trace, expected gain, falsifier, and
    rollback; unsupported hypotheses remain deferred rather than becoming work.
  - Requires 1.2.1.

## 2. Rightsize scheduling and initialization

Idea: smaller host-owned budgets and capability-aware construction can reduce
per-process cost without removing tools or weakening session isolation.

### 2.1. Select a runtime budget empirically

- [ ] 2.1.1. Compare default, one-worker, and two-worker multi-thread runtimes.
  - Preserve or explicitly document environment/configuration precedence and
    report effective workers; do not confuse one worker with current-thread.
  - Success: repeated density runs identify a winner or retain the default;
    unrelated sessions and cancellation remain responsive under tool load.
  - Requires 1.2.2; see
    [RFC section 4.1](../rfcs/rfc-0004-diapason-acp-hot-path.md).

- [ ] 2.1.2. Bound expensive blocking admission and audit stack requirements.
  - Add only evidence-supported workload limits before blocking submission;
    include cancellation, bounded pending work, and child cleanup.
  - Success: saturation cannot create an unbounded queue or starve cancellation;
    stack reductions, if any, pass deep-input/large-future target tests.
  - Requires 1.2.2; keep this patch separate from 2.1.1 for attribution.

### 2.2. Remove unnecessary setup from the critical path

- [ ] 2.2.1. Select tool capabilities before expensive handler construction.
  - Preserve stable tool discovery, later selection, permissions, and scoped
    cache invalidation; reuse existing immutable catalogue projections.
  - Success: equivalent model tools and semantic traces require less measured
    setup work, without sharing mutable state between unrelated workspaces.
  - Requires 1.2.2; see
    [RFC section 4.2](../rfcs/rfc-0004-diapason-acp-hot-path.md).

- [ ] 2.2.2. Evaluate bounded optional setup and safe deferral.
  - Test MCP/subagent setup independently; retain eager MCP discovery unless
    versioned trusted descriptors preserve tool visibility and authorization.
  - Preserve #187's provenance/approval boundary and report degraded providers.
  - Success: a narrow implementation improves total first-use latency, or a
    documented no-change result explains why safe deferral is not worthwhile.
  - Requires 2.2.1; no MCP sharing daemon or blanket lazy connection rewrite.

## 3. Bound and optimize demonstrated forwarding hotspots

Idea: explicit queue ownership and small measured changes improve
responsiveness without dropping semantic events or concealing backpressure.

### 3.1. Establish lossless flow control and lifecycle ownership

- [ ] 3.1.1. Implement tested item/byte budgets at measured queue boundaries.
  - Count in-flight payloads and pending steering; specify oversize rejection,
    replaceable progress, maximum flush delay, and explicit overload outcomes.
  - Success: slow/stopped readers and bursty tools obey the bounds while text,
    tool arguments, approvals, usage, and terminal results remain lossless.
  - Requires 1.2.2; see
    [RFC section 4.3](../rfcs/rfc-0004-diapason-acp-hot-path.md).

- [ ] 3.1.2. Prove cancellation and durable shutdown under pressure.
  - Keep cancellation actionable independently of saturated normal output;
    own tasks, bounded writes, required checkpointing, and process reaping.
  - Success: cancellation/EOF/provider-failure tests terminate within the
    declared budget and never report successful delivery to a wedged client.
  - Requires 3.1.1; test concurrent interleavings, not only sequential models.

### 3.2. Change only evidenced hot operations

- [ ] 3.2.1. Remove measured redundant projection, copying, or lock-held work.
  - Preserve cache identities, exact stream content, tool IDs, and ordering.
  - Success: the targeted metric clears the preregistered materiality gate and
    unaffected workloads stay within regression limits; otherwise revert.
  - Requires 1.2.2 and the relevant 3.1 boundary tests.

## 4. Qualify the selected operating envelope

Idea: the combined release must retain the gains under real process density,
not just pass isolated microbenchmarks.

### 4.1. Publish evidence and guard the result

- [ ] 4.1.1. Repeat the full paired workload and density matrix.
  - Record raw samples, uncertainty, PSS, process-tree memory, worker/queue
    counts, CPU, cancellation, first-use latency, and semantic equivalence.
  - Success: the accepted combination meets section 5's gates on the tested
    host envelope; unavailable 50-process evidence remains explicitly absent.
  - Requires accepted phase 2-3 changes; no allocator switch in this comparison.

- [ ] 4.1.2. Document defaults, overrides, rollback, and regression checks.
  - Add deterministic CI bounds/contract tests; keep noisy absolute timing
    thresholds out of shared runners unless the environment is controlled.
  - Rebaseline after `kthxbai` or `ytmnd` changes the runtime envelope.
  - Success: operators can identify effective budgets and restore the previous
    configuration; diagnostics remain bounded and content-free by default.
  - Requires 4.1.1.
