# Diapason observability roadmap

This roadmap translates [RFC 0002](../rfcs/rfc-0002-diapason-observability.md)
and the [technical design](../design/diapason-observability-design.md) into
review-sized delivery work for Diapason. It does not promise dates. Phases are
GIST ideas to validate or falsify; steps are workstreams that answer sequencing
questions; numbered tasks deliver measurable changes.

## 1. Ratify the telemetry contract

Idea: if maintainers settle signal ownership, version compatibility, privacy
and delivery boundaries before runtime integration, later features can share
one coherent contract. If a supported Rust/exporter combination cannot meet the
lifecycle and resource bounds, revise the contract before enabling export.

### 1.1. Settle the v1 choices that constrain every slice

This step determines which integrations and trust boundaries later work may
rely on. The result informs the crate boundary, configuration and release
claims. See the
[design §§3-4, 8, 10, 12](../design/diapason-observability-design.md).

- [ ] 1.1.1. Record the v1 SDK, exporter and TLS compatibility decision.
  - Verify the candidate OTel dependency family against the declared Rust 1.93
    minimum, locked
    builds, supported targets and the selected HTTP/gRPC TLS implementations.
  - Success: an accepted ADR records versions, enabled transports and every
    deliberately excluded feature.
  - See
    [design §3](../design/diapason-observability-design.md#3-architecture-and-rust-ownership).

- [ ] 1.1.2. Ratify signal ownership and schema evolution rules.
  - Assign a single owner to each provider, token, retry, tool and terminal
    measurement; pin the GenAI convention revision and projection version.
  - See [RFC 0002 §§5-6](../rfcs/rfc-0002-diapason-observability.md).
  - Success: an instrument registry gives each name, type, unit, owner,
    observation boundary, attribute allowlist and compatibility policy.

- [ ] 1.1.3. Define managed configuration authority and activation rules.
  - Specify trusted sources, precedence, locked fields, CLI opt-in and
    unconditional disable behaviour.
  - Success: an ADR resolves the open questions in
    [design §8.2](../design/diapason-observability-design.md) with adversarial
    workspace and environment cases.
  - See
    [design §8](../design/diapason-observability-design.md#8-privacy-configuration-and-trust).

- [ ] 1.1.4. Approve the privacy classification and evidence boundary.
  - Define prohibited default fields, content categories, identifier policy,
    retention and the distinction between telemetry and durable audit.
  - Success: each proposed field has a classification, purpose, source and
    signal destination; no raw content or secret is implicitly allowed.
  - See [design §§7-8](../design/diapason-observability-design.md).

### 1.2. Establish independently testable telemetry contracts

This workstream answers whether the design can be represented without making
runtime crates depend on exporters. Its types and registries unlock the first
end-to-end signal slice.

- [ ] 1.2.1. Define versioned safe observation and projection records.
  - Keep `ThreadEvent` authoritative and make exported log projection additive.
  - Include evidence provenance, event time and observation time.
  - Success: representative lifecycle, usage, policy and gate records validate
    against one versioned schema without content fields.
  - See [design §§3-4, 7, 10](../design/diapason-observability-design.md).
  - See
    [design §3](../design/diapason-observability-design.md#3-architecture-and-rust-ownership).
  - Requires 1.1.2 and 1.1.4.

- [ ] 1.2.2. Define metric dimensions, buckets and cardinality limits.
  - Include GenAI inference/token mappings and custom operational instruments.
  - Success: every instrument has a finite dimension policy and an aggregation
    budget that bounds the declared series and bucket maximum.
  - See [design §6](../design/diapason-observability-design.md).
  - See
    [design §6](../design/diapason-observability-design.md#6-metrics-tokens-and-cost).
  - Requires 1.1.1 and 1.1.2.

## 2. Export one useful interaction safely

Idea: if a user can opt in and inspect one interaction through traces, metrics
and structured lifecycle logs with all signals independently selectable, the
telemetry boundary is useful before detailed instrumentation grows. Failure in
one signal must leave the other signals and command result intact.

### 2.1. Show operators the effective telemetry state

This step answers which settings actually govern export and whether an operator
can diagnose configuration without provider authentication. It unlocks safe
local collection and managed deployment review. See
[design §8](../design/diapason-observability-design.md).

- [ ] 2.1.1. Add nested `telemetry.otel` configuration and validation.
  - Preserve the semantics of trajectory, dashboard and debug-tracing settings.
  - Reject unknown keys, invalid endpoints, unsafe protocols and conflicting
    managed constraints before SDK creation.
  - Success: the default configuration starts no exporter or network request.
  - See
    [design §8](../design/diapason-observability-design.md#8-privacy-configuration-and-trust).
  - Requires 1.1.3 and 1.1.4.

- [ ] 2.1.2. Add `diapason telemetry status --json` and `doctor`.
  - Report effective signals, safe endpoint origins, source precedence, schema
    versions and last local export health without revealing header values.
  - Success: both commands work without provider credentials; `doctor` makes
    no network request and stdout remains valid JSON.
  - See
    [design §8](../design/diapason-observability-design.md#8-privacy-configuration-and-trust).
  - Requires 2.1.1.

- [ ] 2.1.3. Compose local and OTel layers in one owned runtime lifecycle.
  - Add proposed `diapason-telemetry` boundary and binary bootstrap/shutdown
    ownership; retain the file, TUI and error-collector layers.
  - Success: a second global subscriber is diagnosed and cannot panic or
    silently replace an embedding application's providers.
  - See [design §§2-3, 9](../design/diapason-observability-design.md).
  - Requires 1.1.1, 1.2.1 and 2.1.1.

### 2.2. Complete a metadata-only interaction trace

This step answers whether a real CLI interaction yields correctly related
signals through a collector. Its outcome informs where deeper spans belong.

- [ ] 2.2.1. Export one interaction and canonical lifecycle events over OTLP.
  - Implement trace, metric and log exporters with independently selectable
    signals and the accepted HTTP/protobuf transport. Include bounded queues,
    capture filtering and shutdown from first activation; phase 6 proves them
    under saturation rather than introducing safety bounds later.
  - Success: a collector fixture decodes an interaction root, session lifecycle
    records and terminal outcome with `content=none`. Before first opt-in,
    fixtures prove no-secret projection, managed destination/redirect/proxy/TLS
    enforcement, exporter failure isolation, and bounded queues, active spans
    and shutdown. Later phases broaden these cases; they do not defer them.
  - Requires steps 1.1-1.2 and task 2.1.3.
  - See [design §§3, 5, 7](../design/diapason-observability-design.md).

- [ ] 2.2.2. Add the configured gRPC transport where approved in task 1.1.1.
  - Success: the same fixture receives equivalent records over gRPC, and a
    disabled signal sends no requests on either transport.
  - Requires 2.2.1.
  - See [design §§8.2, 10](../design/diapason-observability-design.md).

- [ ] 2.2.3. Instrument interaction and turn completion with owned spans.
  - Use async-safe `Instrument`; prove each operation reaches one terminal
    state under success, error, cancellation and drop.
  - Success: interleaved interactions never inherit each other's IDs, and
    failed commands do not acquire success status.
  - Requires 2.2.1.
  - See [design §§3.1, 5, 11](../design/diapason-observability-design.md).

## 3. Explain model usage, retries and cost

Idea: if model operations expose comparable latency, outcomes and usage across
providers while preserving uncertainty, operators can explain the largest
variable cost of an agent run. If normalized usage cannot be reconciled to
independent provider attempt fixtures, defer cost totals until ownership is
correct.

### 3.1. Follow one inference through streaming and retry

This step resolves logical-call versus physical-attempt semantics before tool
and workflow totals rely on them. See
[design §§5-6](../design/diapason-observability-design.md).

- [ ] 3.1.1. Instrument provider dispatch and each physical attempt.
  - Record provider/model, safe transport outcome and attempt duration at the
    actual adapter call site.
  - Success: an outer task retry and a provider retry produce distinct
    dimensions and IDs, each incrementing only its own retry counter.
  - Requires 2.2.3.
  - See
    [design §5](../design/diapason-observability-design.md#5-trace-topology-and-operation-semantics).

- [ ] 3.1.2. Measure inference duration and first qualifying output chunk.
  - Keep the logical span through stream completion, retry backoff and known
    usage; omit time-to-first-chunk if no non-empty output arrives.
  - Success: a deterministic stream fixture verifies start, chunk and terminal
    timestamps against a monotonic clock.
  - Requires 3.1.1.
  - See
    [design §5](../design/diapason-observability-design.md#5-trace-topology-and-operation-semantics).

- [ ] 3.1.3. Normalize token usage exactly once across provider frames.
  - Handle cumulative and incremental snapshots, duplicate final frames,
    partial failed attempts, cached and reasoning subsets, and unknown modality.
  - Success: generated frame sequences match an independent attempt ledger;
    subset counters never double the input/output totals.
  - Requires 3.1.1.
  - See
    [design §6](../design/diapason-observability-design.md#6-metrics-tokens-and-cost).

- [ ] 3.1.4. Report cost with source, price revision and unknowns.
  - Distinguish provider-reported cost from estimates; preserve unpriced or
    partial operations and avoid mixing currency bases.
  - Success: the same consumer query can separate reported, estimated and
    unknown amounts by logical operation without adding subsets twice.
  - Requires 3.1.3.
  - See [design §6.2](../design/diapason-observability-design.md).

- [ ] 3.1.5. Reconcile current metrics and session summaries with new owners.
  - Feed existing JSON and Prometheus projections from the single observation
    owner during migration.
  - Success: fixed workload totals match between old and new projections, with
    no aggregate feeding back as an additional observation.
  - Requires 3.1.3.
  - See [design §§2, 6](../design/diapason-observability-design.md).

## 4. Explain tool work and safety decisions

Idea: if a tool call can be followed from policy evaluation through approval,
enforcement and result, teams can inspect agent behaviour without confusing
intended permissions with executed actions. The slice tests whether policy
telemetry remains useful under denial and exporter failure.

### 4.1. Relate tool execution to model intent and authority

This step answers where time was spent and which decision allowed or blocked
execution. It unlocks security and agent-delegation analysis. See
[design §§5, 7](../design/diapason-observability-design.md).

- [ ] 4.1.1. Trace tool, MCP and hook execution at their owning boundaries.
  - Reconcile the existing MCP span with one logical tool operation; capture
    only bounded tool identity, result class and duration.
  - Success: one call has no duplicate protocol span and one terminal count.
  - Requires 3.1.1.
  - See
    [design §5](../design/diapason-observability-design.md#5-trace-topology-and-operation-semantics).

- [ ] 4.1.2. Separate policy evaluation, approval wait and enforcement.
  - Record decision ID, source/revision, wait duration, final decision and
    observed sandbox result as distinct events/spans.
  - Success: allow, deny, ask, timeout and cancellation fixtures preserve their
    distinct outcomes; export failure changes none of the decisions.
  - Requires 4.1.1.
  - See [design §§7, 11](../design/diapason-observability-design.md).

- [ ] 4.1.3. Trace delegated agents, compaction and recovery lineage.
  - Use children for synchronous work and bounded links for detached,
    resumed or fan-in work.
  - Success: compaction records counts but no summary text; resumed sessions
    have a new process identity linked to the prior session evidence.
  - Requires 2.2.3.
  - See [design §§4.1, 5](../design/diapason-observability-design.md).

- [ ] 4.1.4. Verify child-process propagation preserves the safety boundary.
  - Test allowlisted trace context through configured tools/hooks while proving
    endpoint headers and unrelated environment are removed.
  - Success: propagation disabled by default and never changes sandbox/effect
    decisions or grants network access.
  - Requires 4.1.2 and 4.1.3.
  - See [design §§4.1, 8.2, 11](../design/diapason-observability-design.md).

## 5. Join agent work to workflow outcomes

Idea: if supplied external identities and evidence survive retries, restart and
review boundaries, consumers can explain a programming task end to end. The
slice fails if it must infer current success from stale or unproven telemetry.

### 5.1. Preserve exact correlation and evidence provenance

This step asks whether independent orchestration systems can add their IDs
without importing their packages or changing Diapason's runtime contract. Its
outcome informs the compatibility projection. See
[design §§4, 7, 10](../design/diapason-observability-design.md).

- [ ] 5.1.1. Carry authenticated objective, run, task and attempt identities.
  - Validate ingress and bind IDs to events, spans and logs; keep all IDs out
    of metric labels. Preserve distinct workflow task attempt, agent invocation,
    launcher process ID and positive one-based CLI attempt ordinal identities.
  - Success: missing IDs remain absent; malformed/oversized IDs and non-positive
    CLI attempt ordinals are rejected with a bounded diagnostic; retry fixtures
    retain invocation/task-attempt IDs while changing process ID and ordinal.
  - Requires 1.2.1 and 4.1.3.
  - See
    [design §4](../design/diapason-observability-design.md#4-identity-correlation-and-evidence).

- [ ] 5.1.2. Add gate, review, candidate and artifact evidence adapters.
  - Preserve issuer, observed time, candidate identity and source confidence;
    do not parse agent prose as a gate result.
  - Success: stale candidate evidence cannot be queried as current success;
    imported evidence remains distinguishable from runtime observations.
  - Requires 5.1.1.
  - See [design §§4, 7, 11](../design/diapason-observability-design.md).

- [ ] 5.1.3. Validate consumer joins against real workflow fixtures.
  - Build fixture projections from Dakar, df12-build and Corbusier business
    needs without runtime dependencies on those repositories.
  - Success: fixtures navigate task to attempt, model usage, tool/policy event,
    gate result and artifact reference; estimated cost remains distinct.
  - Requires 5.1.1 and 5.1.2.
  - See [RFC 0002 §7](../rfcs/rfc-0002-diapason-observability.md).

## 6. Operate telemetry under privacy and failure

Idea: if export stays bounded and visible during content opt-in, collector loss
and signal combinations, operators can make a deliberate privacy decision and
the CLI remains usable. Failure to demonstrate either property means the
exporter configuration is not ready for release.

### 6.1. Enforce content and destination controls before enqueue

This step answers whether only trusted settings can authorize data flow. It
reduces the risk created by adding external destinations and sensitive fields.
See [design §8](../design/diapason-observability-design.md).

- [ ] 6.1.1. Enforce managed destination, protocol and signal constraints.
  - Test hostile workspace config, environment overrides, endpoint redirects,
    proxy settings and invalid TLS against managed policy.
  - Success: no rejected configuration sends an exporter request; diagnostics
    reveal no credentials.
  - Requires 1.1.3 and 2.1.1.
  - See
    [design §8](../design/diapason-observability-design.md#8-privacy-configuration-and-trust).

- [ ] 6.1.2. Add optional redacted content records behind trusted opt-in.
  - Implement category selection, pre-enqueue redaction and byte/attribute
    limits; keep private reasoning and secrets excluded.
  - Success: seeded sensitive values do not occur in decoded output or
    exporter buffers after rejection/truncation.
  - Requires 1.1.4, 5.1.1 and 6.1.1.
  - See [design §8.1](../design/diapason-observability-design.md).

### 6.2. Keep operations responsive during exporter failure

This workstream tests the queue, async ownership and exit path together. Its
results determine whether the configured budgets are safe for interactive,
headless and embedded runtimes.

- [ ] 6.2.1. Bound exporter queues, records, retries and diagnostics.
  - Keep per-signal budgets isolated and make drops visible locally without
    recursively exporting exporter errors.
  - Success: saturated queues remain within the accepted RSS budget and command
    output reports loss without blocking a tool or policy decision.
  - Requires 2.2.1.
  - See [design §9](../design/diapason-observability-design.md).

- [ ] 6.2.2. Implement explicit bounded flush and shutdown ownership.
  - Finalize canonical session persistence first, then stop and join workers
    within one shared deadline.
  - Success: stalled HTTP and gRPC collectors do not extend process exit beyond
    the configured deadline plus scheduling allowance.
  - Requires 6.2.1.
  - See [design §§3.1, 9, 11](../design/diapason-observability-design.md).

- [ ] 6.2.3. Verify privacy, signal, entrypoint and failure combinations end to
      end.
  - Cover all eight signal-enable combinations plus content on/off, transport,
    interactive/headless/ACP, replay, subagent, MCP and cancellation cases.
  - Success: pairwise suite covers the declared surface and mandatory adversary
    cases; no exporter failure changes command, policy or session outcome.
  - Requires 4.1.4, 5.1.3, 6.1.2 and 6.2.2.
  - See [design §11](../design/diapason-observability-design.md).

## 7. Demonstrate parity and release the contract

Idea: if independent consumers can reconstruct representative agent work from
collector evidence and the feature meets declared resource budgets, the core
telemetry promise is viable. If any required signal, correlation or failure
case remains opaque, document the gap and keep the release claim narrower.

### 7.1. Prove parity and publish migration boundaries

This step answers whether supported consumers can use the stable evidence
contract without a vendor dashboard. It informs the first release support
matrix and future convention updates. See
[design §§10-12](../design/diapason-observability-design.md).

- [ ] 7.1.1. Publish operator, privacy, schema and troubleshooting guidance.
  - Document configuration precedence, signal setup, data excluded by default,
    loss semantics, supported OTel convention revision and example collector.
    Update development and quick-reference guides, generated config help and
    the documentation index alongside each delivered surface.
  - Success: examples are parsed by config/fixture tests and the documented
    status output matches a real runtime.
  - See
    [design §10](../design/diapason-observability-design.md#10-compatibility-and-schema-evolution).
  - Requires 6.2.3.

- [ ] 7.1.2. Compare disabled and metadata-only performance on a fixed workload.
  - Measure CPU, instrumentation latency, RSS at queue saturation and thread
    count with reproducible build/machine details.
  - Success: disabled mode has no exporter workers/network access; enabled mode
    meets the provisional budgets in design §11 or records an approved change.
  - Requires 6.2.3.
  - See
    [design §11](../design/diapason-observability-design.md#11-verification-and-acceptance-gates).

- [ ] 7.1.3. Run collector-based parity queries and record known gaps.
  - Reconstruct retries, subagent work, approval denial, resumed task, token
    accounting and unknown cost from OTLP fixtures.
  - Success: each query identifies evidence source and missing telemetry and
    does not report invocation as verified task success.
  - Requires 6.2.3 and 7.1.2.
  - See
    [design §11](../design/diapason-observability-design.md#11-verification-and-acceptance-gates).

- [ ] 7.1.4. Migrate the isolated event emitter and existing metric consumers.
  - Preserve old local output until comparison fixtures prove one source per
    measurement; publish any necessary name/semantics migration.
  - Success: combined old/new configuration produces no duplicate event spans
    or counters, and the compatibility policy identifies removal criteria.
  - Requires 7.1.3.
  - See [design §10](../design/diapason-observability-design.md).

## 8. Deferred extensions after the v1 evidence contract

Idea: if the metadata-first core is useful and its failure/privacy properties
hold, broader storage and analytics can be assessed by adoption evidence
without making the CLI responsible for an unreviewed data platform.

### 8.1. Evaluate durable or specialised telemetry only against new requirements

These workstreams remain outside v1. Their outcome should be a separately
reviewed decision, not implicit growth of the core exporter.

- [ ] 8.1.1. Evaluate a durable local spool for identified outage requirements.
  - State threat model, encryption, quota, deletion and replay deduplication.
  - Requires phase 7 completion.
  - See [design §§9, 12](../design/diapason-observability-design.md).

- [ ] 8.1.2. Evaluate HTTP/JSON, exporter plugins, billing reconciliation,
  profiling, hosted multi-tenancy and quality analytics.
  - Define separate consent, authenticated tenant isolation, retention and
    outcome-verification contracts before proposing implementation.
  - Requires phase 7 completion.
  - See [design §§1, 7, 12](../design/diapason-observability-design.md).
