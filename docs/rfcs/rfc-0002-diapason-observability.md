# RFC 0002: Portable OpenTelemetry for agentic work

## Preamble

- **RFC number:** 0002
- **Status:** Proposed
- **Created:** 2026-11-03
- **Audience:** Runtime, provider, safety, operations, and integration
  maintainers.
- **Scope:** A portable tracing, metrics, and structured-event system for
  vtcode. Diapason is the proposed name of the fork; this RFC does not
  implement a rename.
- **Companions:** [Technical design](../design/diapason-observability-design.md)
  and [GIST roadmap](../project/diapason-observability-roadmap.md).

## 1. Summary

This RFC proposes an opt-in OpenTelemetry (OTel) system for vtcode that lets
operators understand agent work from a user interaction through model requests,
tools, approvals, retries, and outcomes. It combines idiomatic Rust `tracing`
spans, typed OTel metrics, and structured lifecycle events, exported through
the OpenTelemetry Protocol (OTLP) to replaceable collectors and backends.

The system targets functional parity with the agentic observability
capabilities of Pi extensions, OpenAI Codex, Goose, and Claude Code. Parity
means comparable visibility and control, not identical schemas or dashboards.
vtcode will retain its runtime event contract, map compatible fields to
evolving GenAI semantic conventions, and preserve business correlations for
consumers such as df12-build, Dakar, and Corbusier.

## 2. Problem

Agentic programming spans many operations whose business meaning crosses
component boundaries. Operators need to explain what happened to a task, what
was retried, which model and tools were used, where time and token cost went,
which policy decisions applied, and what outcome followed. An isolated model
request trace or aggregate counter cannot answer those questions alone.

Portable telemetry also creates a data boundary. Prompts, completions, tool
arguments and results, paths, and safety rationales may expose user or
organization data. Export must be explicitly configured, content capture must
be off by default, and telemetry failure must not change agent or safety
behaviour. Exported business telemetry is not a durable audit record.

## 3. Current state

The binary installs a global `tracing_subscriber` with local formatting,
filtering, and error collection; no OTel provider or exporter is wired into that
[bootstrap](../../src/main_helpers/tracing.rs). The optional `telemetry-otel`
emitter in `vtcode-exec-events` creates isolated spans per event and includes
fields that require privacy review. It does not create a session, interaction,
request, and tool hierarchy. `ThreadEvent` is the existing lifecycle contract
and must remain authoritative.

The
[event contract and emitter](../../crates/common/vtcode-exec-events/src/lib.rs)
and [core metrics](../../crates/codegen/vtcode-core/src/metrics/mod.rs) were
inspected at base `ca5badd345722a15fcf3d891c91a3053fec6fc08` on 2026-11-03.

The core has a `MetricsCollector` with JSON and Prometheus text output. It
tracks provider outcomes and timings, but inspected provider-attempt methods do
not establish production request or retry instrumentation. Session usage and
cost summaries also do not prove per-attempt cost. Existing telemetry
configuration concerns trajectory recording, not OTLP. These foundations should
be reconciled with new instruments rather than counted twice.

## 4. Goals and non-goals

- Goals:
  - Export independently selectable traces, metrics, and structured events
    through standard OTLP transports.
  - Show interactions, turns, inference requests, tool work, policy decisions,
    approvals, subagents, failures, cancellation, and final outcomes.
  - Carry explicit correlation and attempt identities across agentic workflows.
  - Report provider/model, latency, token usage, cache usage, costs where known,
    tool outcomes, and bounded operational measurements.
  - Keep destinations, capture policy, and propagation safe, inspectable, and
    suitable for centrally managed environments.
- Non-goals:
  - Building a backend, dashboard product, billing authority, or policy engine.
  - Treating telemetry delivery as durable storage or an audit guarantee.
  - Exporting sensitive content by default or changing sandbox semantics.
  - Requiring consumers to adopt a specific vendor schema or storage model.

## 5. Proposed design

### 5.1. Rust ownership and signal lifecycle

Add a small proposed `vtcode-telemetry` crate for typed instruments,
configuration validation, privacy filtering, and export lifecycle. The binary
owns initialization and shutdown. It composes existing `tracing_subscriber`
layers with `tracing-opentelemetry` and the OTel SDK/OTLP exporters, without
replacing local logs or the existing error collector. Use typed OTel Meter
instruments directly; do not add a second global metrics recorder bridge.

Each signal can be disabled independently. Configuration supports OTLP/HTTP and
gRPC where enabled, per-signal endpoints, headers, timeouts, and resource
identity. Explicit application configuration has defined precedence over
environment variables; managed configuration may restrict destinations and
content policy. Invalid or unavailable exporters produce an operator-visible
diagnostic and leave local operation available. Queues and shutdown waits are
bounded; flush is best-effort and never delays safety decisions or canonical
event persistence indefinitely.

### 5.2. Traces, events, and metrics

Create interaction-scoped traces with nested turn/attempt, inference-request,
tool, permission wait/decision, hook, and subagent spans. Long-running sessions
may span many interactions, so use explicit session and task correlation
attributes and span links across resumed or detached interactions; do not keep
one unbounded root span open. Use `ThreadEvent` to produce structured lifecycle
events and outcome evidence, then map applicable fields to GenAI conventions.
Trace sampling may reduce trace volume; canonical events are not sampled before
the event recorder. Their OTLP delivery remains best-effort.

Use histograms for durations and counters for completed operations and token
contributions. Keep metric dimensions limited to bounded categories such as
provider, model family or configured model, operation, tool type, outcome, and
error class. Put run, task, session, attempt, workspace, and artifact
identities on spans or event records, never as metric labels. Costs carry
provenance and confidence; unknown prices remain unknown. Never infer task
success from tool invocation or request completion alone.

### 5.3. Correlation and consumer requirements

Model relationships explicitly: logical objective/correlation, workflow
invocation, logical node, node attempt, agent invocation, process, CLI attempt,
session, interaction, provider request, tool call, gate, and artifact. Not
every vtcode run has every entity; fields are optional and consumers may add
their own context. W3C trace context may cross deliberate child-process or
remote boundaries. Do not forward exporter credentials, arbitrary environment,
or untrusted trace baggage into shell commands, hooks, or tools.

The design responds to df12-build's correlation-first contract, Dakar's
distinction between reported usage and estimated cost, and Corbusier's proposed
durable lifecycle and audit model. It supplies compatible evidence and IDs; it
does not claim to implement their collectors, query stores, or workflow
orchestration.

- [df12-build contract](https://github.com/leynos/df12-build/blob/bf31b742caddce6814aa6331adf0b6ea8a62ab02/docs/workflow-observability-contract.md#L47).
- [Dakar usage reporting](https://github.com/leynos/dakar/blob/4284fd45dc360b6297e71981b4af812f940c89ec/scripts/live-review-harness.mjs#L622).
- [Corbusier runtime-state proposal](https://github.com/leynos/corbusier/blob/ebb75e8fd485600320b0aefec9ed3d1b5b2c9459/docs/adr-006-durable-runtime-state-and-audit-model.md#L34).

### 5.4. Privacy, trust, and failure

Default to metadata-only capture. Content export requires explicit opt-in,
redaction, and size limits, and remains separately controllable by signal.
Never export credentials, authorization headers, raw prompts, completions, tool
arguments/results, unrestricted file paths, or policy rationale by default.
Apply filtering before data reaches exporters. Managed policy can pin approved
destinations and prevent developer-level overrides. Propagated context is
allow-listed, size-bounded, and treated as untrusted input. Export failures are
reported locally without recursively exporting diagnostics or changing the
program's result.

## 6. Requirements and parity criteria

Implementation reaches parity when the following behaviours can be observed
through a standards-compatible collector, with each signal enabled or disabled
independently:

| Capability                | Observable acceptance criterion                                                                                                                    |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Agent trace               | One interaction trace relates turn attempts, inference requests, tools, approval waits, and final outcome.                                         |
| Provider usage            | Provider/model, request duration, outcome, input/output and available cache/reasoning usage appear with provenance.                                |
| Tool and safety work      | Tool duration/outcome and permission decision/wait are correlated with the invoking interaction.                                                   |
| Workflow correlation      | Supplied task/run/attempt identities join the relevant spans and structured events; absent identities are not guessed.                             |
| Operational metrics       | Duration, counts, in-flight work, errors, retries, and token contributions use bounded dimensions.                                                 |
| Privacy controls          | Default exports contain no content fields; opt-in content has explicit redaction and limits; managed destinations cannot be silently bypassed.     |
| Portability and lifecycle | A collector can receive selected signals over supported OTLP transports; bounded shutdown reports export loss without changing the command result. |

_Table 1: Observable acceptance criteria for agentic telemetry parity._

The OpenTelemetry GenAI conventions are evolving. At the researched revision,
inference duration and token usage use `gen_ai.client.inference.*` names;
cache-read, cache-write, and reasoning token counts are subsets of totals.
Implementations must pin and document their convention revision, preserve
unknown modality and price, and avoid summing subset counters into totals.
[GenAI conventions at revision 95aff9c](https://github.com/open-telemetry/semantic-conventions-genai/tree/95aff9cdf605bbc7d7e8f7c4745a693bd0c4eea8).

## 7. Evidence and prior art

The following matrix distinguishes the supplied implementations. An unverified
signal is not evidence that a product lacks it. Sources were inspected using
Firecrawl on 2026-11-03; the linked primary documentation governs each product.

| Source system                                    | Evidenced capability                                                                                               | Proposed parity obligation                                                                                             |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| Pi: `pi-otel` and `pi-otel-telemetry` extensions | Interaction/session, model and tool traces; token/tool metrics; selectable signals, sampling and content controls  | Reconstruct interaction/turn/tool relationships; reconcile token usage; exercise signal and capture controls           |
| OpenAI Codex native Rust OTel                    | Subscriber integration, session events, signal-specific export and trace context; bounded opt-in response logging  | One Rust provider lifecycle; safe structured events; trusted propagation and bounded capture                           |
| Goose native OTLP with Laminar/MLflow consumers  | Model, tool and subagent traces through OTLP/HTTP; tutorial evidence of content opt-in                             | Receive traces in independent backends and reconstruct delegated work; do not assume undocumented metrics              |
| Claude Code and Agent SDK                        | Metrics and structured logs; beta tracing of model/tool/approval/hook work; managed configuration and flush limits | Separate wait/execution timings; usage/cost and observed code-change metrics; managed destinations and observable loss |

_Table 2: Evidence-to-capability parity, including extension and native paths._

Pi's cited implementations are extensions: they show session-to-tool traces,
token and tool metrics, selectable signals, sampling, and metadata-only
defaults. Codex's Rust OTel component demonstrates `tracing_subscriber`
integration and session events, with explicit content minimisation. Goose
documents OTLP trace export for model, tool, and subagent work. Claude Code
documents metrics and structured events, optional tracing, managed destination
controls, and short-lived exporter limits. Together these systems support the
capability goals, while their differing schemas and lifecycle choices argue for
a neutral contract.

The consumer evidence extends the feature set beyond model calls: task and
attempt lineage, cost provenance, gates and review, policy decisions, recovery,
artifact references, and final outcomes. Dakar currently reports stage results,
findings, token totals, and estimated cost in its review harness. df12-build's
accepted ADR and contract distinguish workflow, node, agent, process, and CLI
attempt identities; collector and wiring work remain proposed. Corbusier's
durable audit model is proposed and informs lifecycle and retention needs, not
an implemented capability.

Primary sources inspected for the design:

- [Pi OTel](https://github.com/NikiforovAll/pi-otel).
- [Pi OTel telemetry](https://github.com/mprokopov/pi-otel-telemetry).
- [Codex OTel](https://github.com/openai/codex/blob/main/codex-rs/otel/README.md).
- [Goose observability](https://goose-docs.ai/docs/tutorials/laminar/).
- [Claude usage monitoring](https://code.claude.com/docs/en/monitoring-usage).
- [Claude Agent SDK observability](https://code.claude.com/docs/en/agent-sdk/observability).
- [Rust OTel](https://opentelemetry.io/docs/languages/rust/).
- [`tracing-opentelemetry`](https://docs.rs/tracing-opentelemetry/latest/tracing_opentelemetry/).
- [OTLP exporters](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/).

The supplied tutorials and commentary provide use cases, not normative
contracts:

- [Pi extension article](https://prokopov.me/posts/pi-otel-telemetry-extension/).
- [Pi maintainer article](https://nikiforovall.blog/ai/productivity/2026/05/16/pi-otel.html).
- [SigNoz Codex walkthrough](https://signoz.io/docs/codex-monitoring/).
- [running Codex safely](https://openai.com/index/running-codex-safely/).
- [Codex detection engineering](https://www.monad.com/blog/detection-engineering-for-openai-codex-otel).
- [Goose tracing with Jaeger](https://aaif.io/blog/trace-and-analyze-goose-sessions-with-opentelemetry-jaeger-and-clickhouse).
- [Goose OTel example](https://github.com/avillela/goose-otel-enablement).
- [Goose OTel walkthrough](https://adrianavillela.com/post/goose-with-otel/).
- [MLflow Goose integration](https://mlflow.org/docs/latest/genai/tracing/integrations/listing/goose/).
- [Claude OTel community project](https://github.com/ColeMurray/claude-code-otel).
- [Elastic Claude monitoring](https://www.elastic.co/security-labs/blog/claude-code-cowork-monitoring-otel-elastic).
- [privacy commentary](https://aibl.to/blog/claude-code-opentelemetry-not-only-cost-tracking/).

## 8. Compatibility and migration

Keep local tracing and existing event persistence functional when OTel is
disabled or unavailable. First reconcile event fields and measurement
ownership; then add opt-in export and instrument end-to-end operations. Keep
existing JSON and Prometheus surfaces until parity is demonstrated, with one
documented source of each measurement. Introduce schema/version changes
additively and document any changed names or semantics. Do not couple this work
to the proposed vtcode-to-Diapason rename or to the separate proposed
shell-policy change in [RFC 0001](rfc-0001-diapason-shell-policy.md).

## 9. Alternatives considered

| Approach                               | Benefit                               | Cost or limitation                                                                                            |
| -------------------------------------- | ------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Extend only the current event emitter  | Reuses an existing OTel feature seam. | Keeps isolated event spans and does not own provider, metrics, privacy, configuration, or shutdown lifecycle. |
| Adopt one vendor's schema and exporter | May shorten one integration path.     | Narrows consumers and makes portability and cross-provider comparison harder.                                 |
| Build a bespoke telemetry backend      | Controls storage and query behaviour. | Adds a service, retention policy, and operational burden outside the requested portability layer.             |

_Table 3: Alternatives to the proposed portable telemetry layer._

## 10. Open questions

- Ratify HTTP/protobuf by default, feature-gated gRPC and the compatible TLS
  dependency family proposed in the technical design.
- Select the managed configuration source while preserving the design's
  explicit precedence and untrusted-workspace boundary.
- Validate provider measurement ownership against live call paths before
  replacing compatibility outputs; method declarations alone are insufficient.
- Confirm the pinned GenAI revision and versioned migration policy rather than
  silently following upstream development conventions.
- Validate the proposed 2-second shutdown and resource budgets on CLI, ACP and
  embedding runtimes before implementation acceptance.

## 11. Recommendation

Adopt the portable, opt-in design and use the companion technical design and
roadmap to settle implementation contracts. Measure parity against the
acceptance criteria in Table 1, retain `ThreadEvent` as the runtime lifecycle
source, and keep sensitive content out of default exports and high-cardinality
work identities out of metric dimensions.
