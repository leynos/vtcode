# RFC 0005: Diapason allocator spike

## Preamble

- **RFC number:** 0005
- **Status:** Proposed; documentation only, with no allocator change authorized
  solely by merging this RFC.
- **Created:** 2026-11-07
- **Codename:** `ytmnd`
- **Audience:** Runtime, packaging, performance, and release maintainers.
- **Scope:** One short Linux allocator experiment and a conditional minimal
  change.
- **Companions:**
  [Allocator spike roadmap](../project/diapason-allocator-roadmap.md).

## 1. Question, budget, and possible outcomes

Does changing Diapason's allocator or its configuration materially reduce
retained physical memory for its real ACP workload, without unacceptable
latency, CPU, peak-memory, portability, or build costs?

Resurrect the existing analysis and benchmark, not a general memory-management
programme. Cap the work at one engineer-day, defined as eight active hours
including benchmark repair, measurement, analysis, and any minimal conditional
implementation. Elapsed unattended benchmark time must still fit a declared
host-resource budget. Stop at the cap; inconclusive evidence means no default
change. A larger implementation requires separately scoped work.

Permitted outcomes are: retain the allocator and settings each target package
deploys; adopt a measured configuration-only improvement; or select a different
allocator for a demonstrated deployment target. All are successful spike
outcomes if the evidence supports them. No other allocator, general cache
rewrite, leak-hunting programme, worker-pool tuning, or cross-platform
performance campaign belongs in this spike.

## 2. Existing evidence needs rehabilitation

The inspected source baseline is `0c54619cb827e48bf09ea3b68b41cd6d2e72d905`.
The historical allocator report contains macOS measurements and extrapolates
Linux reclamation from an external workload; it does not establish a current
Linux result.[^1] Preserve those numbers as dated reported evidence, not fresh
measurements or portable truth. Recover source revision, hardware, settings,
and raw samples where available; explicitly label missing provenance.

The production selector defaults to mimalloc and offers `allocator-jemalloc`,
with `profiling` taking precedence over both. The release workflow enables
`allocator-jemalloc` for Linux GNU targets and leaves the musl and Windows
packages on the mimalloc default.[^5] The GNU packages therefore already deploy
jemalloc, so mimalloc is not their baseline. The standalone throughput
benchmark duplicates selection without all those conditions or the same
configuration symbol.[^2] A Cargo feature name alone is therefore insufficient
evidence of the active allocator.

The burst benchmark starts a fixed 60-second peak sampler and awaits it before
recording `post_burst`. Consequently that sample can include an unintended idle
interval rather than the immediate end of work. Its explanatory verdict also
assumes jemalloc on Linux independently of the selected allocator, and its
inner task join loop ignores failures.[^3] Repair those measurement defects
before using the benchmark to decide anything.

The configured jemalloc symbol and profiling options require validation against
the exact linked build. The compiled `malloc_conf` requests profiling options
and does not request background reclamation, so reclamation by the deployed GNU
packages must be measured rather than assumed. Compile-time support for
background threads does not by itself prove runtime enablement or successful
purging. Likewise, old prose about unsupported platforms or mimalloc's
retention behaviour is not a substitute for checking the linked versions and
observed settings.[^4]

## 3. Small experimental design

### 3.1. Candidates and controls

Build at most two allocator variants from one commit, lockfile, release
profile, target, and toolchain, plus the single permitted configuration-only
candidate. The baseline is the allocator and settings the target package
actually deploys: jemalloc via `allocator-jemalloc` for Linux GNU, mimalloc for
musl and Windows. Use the normal compiler cache and compile-admission queue. Do
not change optimization flags, dependencies, worker counts, TUI inclusion,
logging, tool catalogue, or session retention between variants.

Compare that deployed baseline against a candidate with explicitly verified
background reclamation settings. On Linux GNU the baseline is already jemalloc,
so the candidate is a configuration change to a shipped allocator rather than a
switch away from mimalloc. Permit at most one additional configuration-only
candidate, chosen and recorded before its deciding runs. That may tune the
deployed allocator or the alternative candidate; it is not an unbounded search
over decay, arena, and cache settings. An extra configuration must not require
another code variant or a new allocator library.

Disable profiling/allocator instrumentation for deciding timing runs. Emit
allocator identity, version, effective settings, and instrumentation state
before measurement. Verify the linked symbol prefix and supported environment
variable, rather than assuming `MALLOC_CONF` controls the vendored allocator.
Where available, query background-thread and decay settings through the
allocator's supported diagnostics outside timed sections. Fail the experiment
on an identity/configuration mismatch.

Use a fixed worker budget. If `shoopdawoop` has not selected one, record the
current effective budget rather than waiting for that programme. Rerun only the
bounded qualification case when a later headless/runtime change alters the
deployment; do not compare measurements from different architectures.

### 3.2. Workloads and sampling

Reuse the repaired burst/idle workload and throughput microbenchmark as
explanatory controls, not as the sole decision. The primary evidence comes from
the real ACP binary with a deterministic local provider, retained tool results,
and production checkpoint/recovery paths. Reuse existing fixtures or a small
process driver; do not wait for a new benchmark framework.

Run a short streaming/tool burst and a longer multi-turn replay with fixed
payloads and histories. Observe immediate post-burst memory and subsequent idle
samples at 1, 5, 30, and 60 seconds while the process remains alive. Then start
another turn to expose refault/latency costs of reclamation. Never exit the
process to manufacture an idle-memory improvement.

Measure resident set size (RSS), proportional set size (PSS), private memory,
CPU, page faults, latency, and threads. Record allocator statistics separately
from operating-system measurements. Shared executable pages are not multiplied
as private memory. Include tool/MCP children in a separate process-tree or
cgroup total; a Rust global allocator switch does not automatically govern all
native allocations or child-process heaps.

Test 1 and 10 agent processes first. Validate finalists at 50 only when the
host-resource ceiling permits it. The decision needs evidence at the actual
intended density; a lower-density-only result supports a limited opt-in
recommendation, not an untested fleet-wide default. Include child/background
threads, peak memory, and idle CPU in that result.

Use at least five fresh-process paired repetitions with alternating allocator
order. Pin CPU/quota, input digest, environment, background load, and workload
completion. The independent sample is the paired run, not each token or each of
its correlated sibling processes. Retain raw measurements, medians, p95
latency, and uncertainty. Missing PSS or cgroup access is reported explicitly,
not replaced by invented values or summed RSS presented as physical usage.

### 3.3. Preregistered decision gate

The following proposed thresholds must be ratified before deciding runs:

- A candidate reduces median per-agent PSS at 30 seconds idle by both at least
  15% and at least 8 MiB in the 10-process realistic replay; the 60-second
  sample and intended-density run must not contradict the benefit.
- Workload completion, semantic events, cancellation, and recovery remain
  equivalent. No retained context, required tool, or durable record disappears.
- P95 forwarding, first useful output, resumed-turn, and tool latency regress
  by no more than 5%; total CPU and peak process-tree memory likewise regress
  by no more than 5%. Record absolute values and measurement noise.
- There is no sustained idle-CPU or thread-growth problem, supported-target
  breakage, sanitizer/profiler-selection ambiguity, or unacceptable documented
  build/release cost. Assess background-thread multiplication at host density.

An uncertain or noisy comparison is inconclusive, not a pass. Do not lower
thresholds after seeing the result. A pre-measurement amendment must explain
its operational rationale. Historical microbenchmark speed differences do not
constitute an accepted latency trade-off for the current ACP workload.

## 4. Conditional implementation and rollback

If the candidate clears every gate within the time box, implement only the
winning selection/configuration and necessary diagnostics/test corrections.
Prefer explicit Linux release/deployment feature selection on the validated
target, leaving other platforms and the general Cargo default unchanged unless
a similarly small target-safe mechanism is justified. Unsupported platforms
must never silently receive an unmeasured allocator or misleading label.

Use a single tested selection policy for production and its benchmark, or a
small explicit matrix that proves equivalent conditions. Cover defaults,
`allocator-jemalloc`, profiling precedence, and supported platform
combinations. Do not combine the switch with an allocator upgrade or broad
refactor. Preserve safe pre-initialization configuration and keep allocator
diagnostics off ACP stdout. Recheck the winning binary after the packaging
change.

Record the exact build feature and settings needed to restore the target
package's deployed allocator, and retain a known baseline artifact for
deployment rollback. Do not implement automatic in-process allocator swapping
or periodic forced purges. If the switch cannot fit the time box, retain the
result as a recommendation and scope any larger implementation separately; do
not declare a production switchover complete.

On no-go or inconclusive results, retain current production defaults, publish
the evidence and limitations, and keep only independently useful benchmark
repairs. Remove losing experimental scaffolding. Update overconfident Linux and
macOS assertions in the old analysis without erasing historical results.

## 5. Completion evidence

A compact report records the question, spent effort, commit and build identity,
raw data, workload, platform, effective allocator, decision gates, observed
trade-offs, conclusion, and rollback. It must state whether the change was
implemented or merely recommended. A retained-default result closes the spike
as decisively as a successful switch. No follow-on tuning project is implied.

## 6. References

[^1]: [Historical allocator analysis](../development/ALLOCATOR_MEMORY.md).
[^2]: [Production selection](../../src/allocator.rs) and
    [throughput benchmark](../../benches/allocator_throughput.rs).
[^3]: [Burst/idle benchmark](../../src/cli/bench_allocator.rs).
[^4]: [jemalloc runtime configuration](https://jemalloc.net/jemalloc.3.html)
    and [mimalloc environment options](https://microsoft.github.io/mimalloc/environment.html).
    Check the locked native versions; current manuals alone do not identify
    the behaviour of a vendored build.
[^5]: [Linux and Windows release build](../../.github/workflows/build-linux-windows.yml)
    selects `allocator-jemalloc` for Linux GNU targets only.
