# Allocator switchover spike roadmap

This roadmap delivers [RFC 0005](../rfcs/rfc-0005-diapason-allocator-spike.md),
codename `ytmnd`. Status: proposed; no new measurements or allocator switch
ship with this plan. Phases are decision gates, steps are bounded workstreams,
and numbered tasks have observable acceptance criteria.

The total budget is one engineer-day, at most eight active hours including
conditional implementation. At most two allocator builds and one additional
configuration-only candidate are permitted. Record effort as work proceeds.
At the cap, stop and publish the current conclusion; do not extend the spike
implicitly. No-go and inconclusive are valid completed outcomes.

## 1. Restore a trustworthy experiment

Idea: a small repair of existing evidence and measurement tools can answer the
question without establishing a new performance programme.

### 1.1. Recover provenance and fix measurement defects

- [ ] 1.1.1. Audit the old report and active allocator identity.
  - Recover available revisions, hardware, settings, and raw samples; label
    missing provenance and distinguish macOS measurements from Linux inference.
  - Inspect selector/platform/profiling differences between binary and bench,
    the linked allocator version, configuration symbol, and effective purging.
  - Success: an experiment record names the actual baseline and no inherited
    sentence masquerades as a new Rohga result.
  - See [RFC section 2](../rfcs/rfc-0005-diapason-allocator-spike.md).

- [ ] 1.1.2. Repair the existing burst/idle measurement boundary.
  - Stop and join peak sampling at burst completion; sample immediate memory
    before the explicit idle phase. Keep measurements lightweight and label
    any instrumentation overhead that could disturb worker idleness.
  - Propagate nested task failures and report the effective allocator rather
    than selecting a verdict from the operating system alone.
  - Success: deterministic timing/failure tests reject the former hidden
    60-second wait and false allocator label; failed workloads yield no verdict.
  - Requires 1.1.1.

### 1.2. Lock the limited candidate and workload matrix

- [ ] 1.2.1. Ratify thresholds, controls, and the host-resource ceiling.
  - Freeze worker count, build profile, input/commit/lockfile, repetition, and
    memory/latency/CPU gates before collecting deciding measurements.
  - Select mimalloc versus verified jemalloc reclamation, with at most one
    preregistered configuration-only candidate; no extra allocator or upgrade.
  - Success: the record contains a hard stop time/effort, a falsifier, and the
    exact conditions for a go, no-go, or inconclusive outcome.
  - Requires 1.1.1; see [RFC section 3](../rfcs/rfc-0005-diapason-allocator-spike.md).

## 2. Measure realistic ACP work and decide

Idea: physical-memory savings matter only when they survive another real turn
and do not consume equivalent resources through CPU, faults, or threads.

### 2.1. Compare the bounded candidates

- [ ] 2.1.1. Run paired synthetic and production-path workloads.
  - Build at most the two variants through the normal shared cache/admission
    path; reuse local mock-provider ACP fixtures without paid inference.
  - Collect at least five paired fresh-process repetitions, burst/idle
    samples, and resumed-turn latency at 1 and 10 processes.
  - Success: raw data identifies allocator/settings and separates parent PSS,
    child/cgroup memory, latency, CPU, faults, and threads without dropped work.
  - Requires 1.1.2 and 1.2.1.

- [ ] 2.1.2. Qualify the finalist at intended density and record the decision.
  - Run the bounded 50-process case only if safe; report unavailable capacity
    rather than extrapolate a fleet-default claim from a smaller experiment.
  - Compare the preregistered materiality/regression gates with uncertainty.
  - Success: a short decision record concludes go, no-go, or inconclusive and
    reports effort used; only a go opens the conditional implementation task.
  - Requires 2.1.1; stop at the remaining effort/resource budget.

## 3. Apply a winning change or close with retained defaults

Idea: a useful spike ends in a small operational decision, not a continuing
allocator research project.

### 3.1. Implement only the evidence-supported outcome

- [ ] 3.1.1. Apply and verify the minimal winning selection, only on go.
  - Limit the change to validated Linux packaging/configuration and necessary
    identity/selection tests; preserve other platforms and profiling behaviour.
  - Retest the final artifact and document the exact mimalloc rollback.
  - Success: the packaged candidate still clears the gates and fits the total
    time box. Otherwise retain defaults and record a recommendation only.
  - Requires a go from 2.1.2 and sufficient remaining budget.
  - See [RFC section 4](../rfcs/rfc-0005-diapason-allocator-spike.md).

- [ ] 3.1.2. Publish the report and retire unsupported claims in every outcome.
  - Preserve historical numbers with provenance labels; correct the old report
    and benchmark guidance, remove losing scaffolding, and retain useful tests.
  - On no-go/inconclusive, record 3.1.1 as not applicable, not unfinished work.
  - Success: the report links raw evidence, scope, effort, trade-offs, outcome,
    implementation status, and rollback; production defaults remain unchanged
    unless the complete go path passed.
  - Requires 2.1.2, or an explicit time-box stop with partial evidence.
