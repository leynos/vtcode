# Diapason shell policy roadmap

This roadmap turns [RFC 0001](../rfcs/rfc-0001-diapason-shell-policy.md) and the
[technical design](../design/diapason-shell-policy-design.md) into
review-sized delivery slices. It makes no date commitments. It follows Goals,
Ideas, Steps, and Tasks (GIST): each phase is a testable idea, each step is a
workstream that reduces a delivery risk or unlocks a slice, and each task
delivers an observable result.

The goal is one authorization boundary that can represent all existing command,
tool, Model Context Protocol (MCP), approval, and sandbox-related decisions,
analyse supported Bash without executing it, and retain explicit uncertainty.
The project can retire existing authorities only when every route has moved and
the required evidence passes. Success is measured against RFC 0001
[requirements R1–R9](../rfcs/rfc-0001-diapason-shell-policy.md#5-requirements-and-acceptance-criteria):
all legacy decisions represented, unknowns fail conservatively, lower
authority cannot weaken higher controls, every route is mediated, execution
remains bound to its authorization, faults block, authors can explain changes,
old decision paths are retired, and routine diagnostics do not expose secrets.

## 1. Foundational contracts and feasibility

Idea: if the Brush and Regorus integration can meet the repository's compiler,
resource, and isolation constraints, and the request/evidence contracts can
encode current behaviour, then later slices can consolidate policy without
weakening authorization. The phase is falsified if either dependency cannot be
embedded safely within bounded workers or the legacy inventory cannot map to
explicit contract fields. Advance only when dependency candidates are pinned or
rejected with evidence, every inventoried route and default has an owner and
fixture, and the schema rejects incomplete authorization input.

### 1.1. Resolve the dependency and worker boundary

This step answers whether the chosen foundations can build at the workspace's
compiler baseline and whether analysis/evaluation can stop at real resource
limits. Its results determine dependency versions and whether the proposal
needs a bounded alternative experiment. See design §§3, 5, 6, and 12; RFC 0001
§§7–8.

- [ ] 1.1.1. Verify Brush parser and Regorus release candidates against the
  workspace baseline.
  - See [design §12][design-12].
  - Record version, minimum supported Rust version (MSRV), licence, required
    Cargo features, compile cost, and unsupported syntax/builtins.
  - Success: pinned candidates build with the repository's locked compiler
    settings, and all unsupported features have explicit activation failures.
- [ ] 1.1.2. Build bounded feasibility prototypes for analysis and policy
  evaluation.
  - See [design §5][design-5].
  - Prototype parser/evaluator and Regorus compilation/evaluation workers at
    proposed source, node, time, and memory bounds; do not implement the full
    abstract interpreter in this feasibility task.
  - Success: the supervisor terminates an over-budget prototype and reports a
    blocking fault without returning partial permission.
- [ ] 1.1.3. Record dependency, feature, and budget decisions.
  - See [design §12][design-12].
  - Specify fallback decision gates if either candidate fails feasibility.
  - See RFC 0001 §§8–9.
  - Success: an accepted decision identifies the pinned candidates and the
    evidence that justified the bounds.

### 1.2. Freeze the current decision surface

This step answers which rules are active, dormant, or enforcement-only and what
each one means. The result is the migration oracle for every later slice. See
design §§9–10 and RFC 0001 §§2, 5–6.

- [ ] 1.2.1. Create a route and authority ledger for shell, argv,
  pseudo-terminal (PTY), native, MCP, delegated-agent, safe-mode, and full-auto
  requests.
  - See [design §9][design-9].
  - Record callsite, ordering, default, failure semantics, persistence, cache
    scope, sandbox enforcement, and current test oracle.
  - Include dormant compatibility helpers separately from live decisions.
  - Success: every route in RFC 0001 R4 has a named owner and no unexplained
    authorization callsite remains.
- [ ] 1.2.2. Capture compatibility cases for each rule dialect and default.
  - See [design §9][design-9].
  - Cover raw text versus argv prefixes, regex/glob matching, deny-first versus
    first-match ordering, malformed patterns, environment rules, approval
    scopes, and failure behaviour.
  - Success: expected outcomes are reviewed against current source and can be
    replayed without executing proposed commands.
- [ ] 1.2.3. Ratify the effect-graph, finding, and decision contracts.
  - See [design §4][design-4].
  - Specify IDs, spans, encoding, taint labels, uncertainty, authority
    provenance, constraints, empty results, and fault behaviour.
  - Include schema-version and migration rules.
  - Requires 1.1.3.
  - Success: a versioned schema validates representative shell, native, and
    MCP inputs and rejects missing security-critical fields.

## 2. Explainable policy for existing command requests

Idea: if current command rules can run through Regorus while retaining their
own matching dialect and decision order, users can inspect a single explanation
before semantic shell analysis is available. The phase is falsified by an
unexplained permissive divergence or by a path that still uses a second
decision authority after cutover. Advance only when the compatibility corpus
has zero unexplained permissive divergences; otherwise the old evaluator
remains authoritative for that route.

### 2.1. Deliver compatible command decisions in shadow mode

This step answers whether the contract and importer preserve current command
behaviour. Differences inform migration diagnostics and intentional hardening.
See design §§4, 6, and 9; RFC 0001 §§2, 5–6.

- [ ] 2.1.1. Implement the versioned request and finding validators.
  - See [design §4][design-4].
  - Requires 1.2.3.
  - Reject malformed identities, node references, findings, encodings, and
    unknown major versions.
  - Success: invalid and incomplete results block rather than become an empty
    finding set.
- [ ] 2.1.2. Import legacy command rules as source-tagged policy data.
  - See [design §9][design-9].
  - Preserve raw-text and argv matchers, ordering, defaults, environment
    provenance, and exact regex/glob compatibility facts.
  - Keep compatibility matchers pure; they return facts and cannot authorize.
  - Requires 1.2.2, 1.2.3, and 2.1.1.
- [ ] 2.1.3. Run the Regorus evaluator beside the current command path.
  - See [design §6][design-6].
  - Requires 2.1.2 and 2.2.1.
  - Compare decisions and obligations in shadow mode; record classified
    divergences without changing execution.
  - Success: each captured fixture agrees or has a reviewed importer defect,
    uncertainty, legacy defect, or hardening classification.
- [ ] 2.1.4. Add command explanation for rule and compatibility findings.
  - See [design §11][design-11].
  - Show source unit, matcher dialect, matched rule, effective default, and
    remaining obligation with bounded, redacted reasons.
  - Requires 2.1.3.
  - Success: an author can reproduce each fixture with the check/explain
    interface and identify why its result was produced.

### 2.2. Make decision reduction explicit

This step answers how multiple units combine while the command evidence is
still mostly legacy-shaped. It unlocks drop-ins without making match order
depend on filenames. Implement this reducer before running the shadow evaluator
in task 2.1.3. See design §§6 and 10; RFC 0001 §§4–5.

- [ ] 2.2.1. Implement the fixed finding reducer and bounded constraint join.
  - See [design §6][design-6].
  - Requires 2.1.1 and 1.2.3.
  - Preserve `deny`, `review`, `grant`, `constrain`, and `advise` distinctions.
  - Cover conflicts, empty constraints, no-grant cases, and evaluator faults
    alongside the reducer.
  - Success: adding a restriction cannot produce a broader result; conflicts
    and invalid outputs block.
- [ ] 2.2.2. Expose effective-policy and explanation output.
  - See [design §11][design-11].
  - Include generation identity, ordered legacy facts, active rules, and the
    reason for each unsatisfied obligation.
  - Success: output differentiates a denial, missing grant, review, policy
    fault, and unavailable enforcement.
- [ ] 2.2.3. Document the compatible command-policy preview.
  - See [design §11][design-11].
  - Update the safety guide and quick reference with the supported command
    route, migration diagnostics, and shadow-mode limits.
  - See design §11 and RFC 0001 §§5–6.

## 3. Safe policy drop-ins and authorized replacement

Idea: if policy units can be added, tested, and replaced through an explicit
authority graph, maintainers can compose organization, user, and trusted
workspace rules without order-dependent overrides. The phase is falsified if a
lower-trust unit can grant beyond delegation, forge provenance, or weaken an
unreplaced higher-authority restriction. Advance only when forged, stale,
ambiguous, or lower-authority replacements are rejected, and activation is
atomic under fault injection.

### 3.1. Load atomic, isolated policy generations

This step answers whether several sources can contribute policy with stable
identity and reproducible activation. Its outcome enables customization but
does not yet grant workspace files authority. See design §7; RFC 0001 §§4–5.

- [ ] 3.1.1. Implement manifest validation and deterministic source discovery.
  - See [design §7][design-7].
  - Requires 1.2.3 and 2.2.1.
  - Validate schema, declared files, path containment, symlinks, digests,
    duplicate IDs, namespace, parameters, and dependencies.
  - Success: unsafe paths, duplicate units, undeclared files, and unknown
    schemas reject before compilation.
- [ ] 3.1.2. Compile and evaluate each policy unit in an isolated compartment.
  - See [design §6][design-6].
  - Supply only that unit's modules/data and immutable host-built input.
  - Validate Rego v1, builtin profile, required result envelope, and execution
    limits against the pinned Regorus build.
  - Requires 1.1.1, 1.2.3, 2.2.1, and 3.1.1.
  - Success: one unit cannot define another unit's package, read secrets, or
    cause an invalid/undefined output to permit execution.
- [ ] 3.1.3. Activate complete generations atomically.
  - See [design §7][design-7].
  - Requires 3.1.2.
  - Stage, validate, compile, digest, and publish via one generation swap.
  - Keep a valid generation on optional-update failure; block when a required
    source is invalid or revoked.
  - Success: fault injection at each activation stage exposes either the old
    complete generation or the new complete generation, never a mixture.

### 3.2. Deliver additive drop-ins and explicit supersession

This step answers what a unit may change and how a reader can see that change.
It enables trusted local customization while preserving mandatory restrictions.
See design §7 and RFC 0001 §§3–5, 7.

- [ ] 3.2.1. Implement source identity and delegated authority records.
  - See [design §7][design-7].
  - Requires 1.2.3 and 3.1.3.
  - Bind issuer, subject, operation, scope, workspace, expiry, and revocation
    epoch to host trust data.
  - Success: self-delegation, cycles, revoked grants, and expired grants
    reject activation.
- [ ] 3.2.2. Resolve replacement by exact unit identity and digest.
  - See [design §7][design-7].
  - Reject stale/missing targets, cycles, competing replacements, and
    undelegated changes; keep ordinary additions active alongside defaults.
  - Requires 3.2.1 and 3.1.3.
  - Success: a replacement cannot affect any unit beyond its exact authorized
    target, and the effective diff identifies who authorized the change.
- [ ] 3.2.3. Ship policy check, test, effective, and explain commands.
  - See [design §11][design-11].
  - Requires 2.2.2, 3.1.3, and 3.2.2.
  - Validate author fixtures, expected findings, delegation, supersession,
    and redacted explanations without executing proposed actions.
  - Success: add, restrict, replace, stale-target, and unauthorized-replacement
    examples each have a reproducible result.
- [ ] 3.2.4. Document drop-in discovery, trust, and replacement behaviour.
  - See [design §7][design-7].
  - Specify user and opted-in workspace locations, activation failures,
    author fixtures, and how mandatory policy differs from defaults.
  - See design §§7 and 11; RFC 0001 §§4, 6–8.

## 4. Supported Bash requests with semantic evidence

Idea: if a bounded Brush-backed analyser can explain common developer shell
requests without running them and over-approximate supported effects, policy
authors can govern operations instead of relying on command strings. The phase
is falsified if observation on the supported corpus finds an omitted effect, if
exhaustion increases permission, or if the useful subset cannot meet an
approved worker budget. Advance only when the supported-subset corpus contains
no known omitted effect, analysis exhaustion never grants coverage, and
measured workers remain within approved bounds. Anything else stays review-only
or denied.

### 4.1. Analyse controlled shell requests and uncertainty

This step answers whether a request with a deliberately controlled state can
produce a usable graph, explanation, and conservative authorization. Its result
determines which shell constructs can enter the automatic-coverage profile. See
design §§4–5 and §10; RFC 0001 §§4–5.

- [ ] 4.1.1. Adapt Brush syntax to the versioned effect graph.
  - See [design §5][design-5].
  - Requires 1.1.1, 1.2.3, 2.2.1, and 3.1.2.
  - Preserve spans, parse errors, command lists, compound structure, and
    unsupported nodes without tokenization fallback.
  - Success: malformed and unsupported syntax is explicit and cannot produce
    a complete empty graph.
- [ ] 4.1.2. Define the controlled non-interactive Bash state profile.
  - See [design §4][design-4].
  - Construct environment and launch options explicitly; classify startup
    imports, aliases, functions, options, traps, cwd, descriptors, and PATH
    dependencies with provenance.
  - Success: the snapshot emits identities for every dependency consumed by
    analysis and authorization.
- [ ] 4.1.3. Interpret assignments, expansions, field splitting, and command
  resolution in the abstract domain.
  - See [design §4][design-4].
  - Requires 2.2.1.
  - Propagate independent integrity/confidentiality origins and preserve
    quoting as expansion metadata.
  - Include bounded alternatives and widening reasons.
  - Start with scalar assignments, literal words, and ordinary variable
    expansion; unsupported forms remain opaque until a later model extends
    the declared profile.
  - Requires 4.1.1 and 4.1.2.
- [ ] 4.1.4. Record builtins, calls, redirections, and descriptor effects.
  - See [design §4][design-4].
  - Keep redirection and environment effects even when no external process
    launches.
  - Success: `> "$OUT"`, pipelines, command substitutions, and direct argv
    serialize distinct, source-mapped effects.
- [ ] 4.1.5. Explore conditional and pipeline branches with unknown status.
  - See [design §5][design-5].
  - Cover `if`, `&&`, `||`, negation, pipelines, and subshell status joins.
  - Preserve both feasible branches when an external result is unknown.
  - Success: replacing a known status with unknown never removes a reachable
    effect or increases automatic coverage.
- [ ] 4.1.6. Bound loop analysis and widen state at control-flow joins.
  - See [design §5][design-5].
  - Apply the agreed iteration and value-alternative limits; preserve known
    effects and emit an opaque continuation on exhaustion.
  - Success: terminating and non-terminating possibilities remain explicit.
- [ ] 4.1.7. Resolve function calls with definition and invocation provenance.
  - See [design §5][design-5].
  - Preserve positional arguments, local state, recursion bounds, and
    definition-time alias context.
  - Success: unsupported or recursive functions widen to a reachable opaque
    effect without erasing effects already found.
- [ ] 4.1.8. Mark dynamic re-entry, traps, and asynchronous work as opaque.
  - See [design §5][design-5].
  - Bound known `eval`, `source`, and nested shell input; model traps and
    asynchronous jobs where supported.
  - Success: unknown dynamic code is never treated as an empty or unreachable
    program.
- [ ] 4.1.9. Connect graph evidence to policy explanation and review.
  - See [design §6][design-6].
  - Requires 2.2.1 and 3.2.3.
  - Deny known prohibited effects and require a review obligation for uncovered
    or opaque effects under the selected containment profile.
  - Success: users can inspect both possible branches and see which uncertainty
    prevented an automatic decision.

### 4.2. Establish soundness evidence for the supported subset

This step answers what evidence justifies automatic coverage and what stays
review-only. Its corpus and counterexamples set the profile boundary. See
design §§5 and 10; RFC 0001 §§5 and 8.

- [ ] 4.2.1. Compare confined Bash traces with the analyser's supported subset.
  - See [design §10][design-10].
  - Requires tasks 4.1.1–4.1.9.
  - Compare effects for generated bounded programs and adversarial fixtures;
    every observed effect must be represented or covered as opaque.
  - Retain minimized counterexamples as regression fixtures.
  - Success: no known omitted effect remains in the declared subset; the
    documentation states that this is not a proof for arbitrary Bash.
- [ ] 4.2.2. Implement one narrow Git read-command effect model.
  - See [design §5][design-5].
  - Requires 4.1.4.
  - Pin the executable identity and supported configuration/environment for
    selected read commands; expose Git helpers, hooks, pagers, and filesystem
    monitors as explicit dependencies or opaque effects.
  - Keep nested `find`, `xargs`, `make`, and interpreter execution as opaque
    examples for policy decisions; do not implement their models in this task.
  - Success: only the declared Git envelope receives model-derived effects;
    a bare executable name never implies purity.
- [ ] 4.2.3. Deliver an end-to-end taint and explanation-redaction corpus.
  - See [design §4][design-4].
  - Requires tasks 4.1.3–4.1.4.
  - Cover environment, pipes, redirections, substitutions, unknown file
    contents, in-memory policy explanations, prompts, terminal output, and
    model context.
  - Success: secret canaries retain confidentiality labels across these paths
    and are redacted from policy explanations, prompts, terminal output, and
    model context. Cache and event persistence redaction is verified in tasks
    5.1.4–5.1.5.
- [ ] 4.2.4. Publish and measure the supported analysis profile.
  - See [design §10][design-10].
  - Requires tasks 4.2.1–4.2.3.
  - Measure cold/warm latency, peak memory, and uncertainty rates within the
    selected worker limits.
  - Success: each automatically covered construct has a model/version and
    evidence; all other constructs remain review-only or denied.

## 5. One authorization boundary across execution routes

Idea: if the same bound decision and enforcement obligations govern every
execution route, the project can retire competing authorities without creating
a shell or MCP bypass. The phase is falsified by any dispatch path that
executes without a current policy generation, valid approval, and available
required containment. Advance only when every inventoried route passes the
denial/bypass matrix, policy/state changes invalidate bindings, and no old
allow path is reachable.

### 5.1. Bind decisions to actual execution

This step answers whether stateful checks can convert a pure evaluation into a
one-use authorization that is still valid at dispatch time. It unlocks route
cutover. See design §§3, 6, and 8; RFC 0001 §§4–6.

- [ ] 5.1.1. Scope approval records to operations and review obligations.
  - See [design §8][design-8].
  - Requires 1.2.3 and 3.1.3.
  - Represent issuer, workspace, expiry, policy epoch, state dependencies,
    sandbox profile, and exact covered obligations.
  - Migrate old approvals as inspectable records with no invented scope.
  - Success: one-shot approval cannot erase denials or authorize changed
    state, operation, or policy generation.
- [ ] 5.1.2. Bind and revalidate the request before dispatch.
  - See [design §7.4][design-7-4] and [design §8.3][design-8-3].
  - Requires 3.1.3.
  - Bind request, graph, state, executable, policy, approval, budget, and
    sandbox identity; pin the policy generation and revocation epoch, then
    serialize the final generation/approval checks and token consumption with
    activation through the generation barrier. Reject token replay.
  - Success: mutations and concurrent generation/revocation changes between
    evaluation and dispatch force re-evaluation or block.
- [ ] 5.1.3. Integrate runtime reservations and sandbox obligations.
  - See [design §8.3][design-8-3].
  - Requires 3.1.3 and 5.1.2.
  - Keep rate limits, budget reservations, approval storage, and operating-
    system enforcement in Rust execution services.
  - Block when required enforcement is unavailable; do not downgrade.
  - Success: races cannot exceed configured counters and a failed reservation
    or sandbox setup never launches a process.
- [ ] 5.1.4. Cache analysis and findings by their complete dependencies.
  - See [design §8.2][design-8-2].
  - Requires 4.1.2, 5.1.2, and 3.1.3.
  - Key syntax, graph, and finding caches separately by source/state/model and
    policy-generation digests; bypass decision reuse for live counters,
    approvals, revocations, or enforcement availability.
  - Success: mutating any consumed dependency invalidates the relevant cache,
    and no raw secret value is persisted in a cache entry.
- [ ] 5.1.5. Record redacted policy lifecycle and execution events.
  - See [design §3][design-3] and [design §11][design-11].
  - Requires 3.1.3 and 5.1.2.
  - Extend `ThreadEvent` for policy activation, decision, approval/reservation
    reference, and execution outcome; record generation and rule/node IDs.
  - Omit raw environments, secret values, and command output from routine
    events and projections.
  - Persist the required redacted decision and authorization references before
    bind and dispatch; failure to persist this pre-dispatch audit record blocks
    dispatch. Record the execution outcome separately after dispatch.
  - Success: an event trace distinguishes decision from execution outcome and
    secret canaries are absent from stored event payloads, logs, and policy
    test reports.

### 5.2. Move all routes to the shared boundary

This step answers whether each tool route supplies equivalent identity,
context, and enforcement facts. A complete route matrix permits retirement of
old allow authorities. See design §§3, 8–10; RFC 0001 §§5–6.

- [ ] 5.2.1. Migrate direct argv and Bash executor routes, including PTY.
  - See [design §8.3][design-8-3].
  - Preserve request origin and separate tool permission from command payload
    permission. Start in guarded opt-in mode, requiring both old permission
    and the new obligations.
  - Requires tasks 5.1.1–5.1.5.
  - Success: every route's denial tests stop at actual dispatch, including
    nested shell invocation.
- [ ] 5.2.2. Migrate native tools, file/network actions, and delegated tools.
  - See [design §8.3][design-8-3].
  - Requires tasks 5.1.1–5.1.5.
  - Start each migrated route in guarded opt-in mode.
  - Represent capabilities and tool arguments in the shared input; keep
    remote-service effects explicit. Normalize requested and granted
    capabilities before handling additional-permission or conflicting
    escalation requests. Authorize executable-backed skills and native
    libraries before initialization, since loading them can execute code.
  - Success: full-auto and preapproval are policy facts and cannot bypass the
    common boundary; unauthorized components never initialize.
- [ ] 5.2.3. Migrate MCP calls and server startup.
  - See [design §8.3][design-8-3].
  - Requires tasks 5.1.1–5.1.5.
  - Include canonical aliases, provider identity, unknown-provider defaults,
    stdio launch, and HTTP/remote limitations in guarded opt-in mode.
  - Success: denying a provider/action or required local sandbox blocks both
    the applicable call and server-launch path.
- [ ] 5.2.4. Exercise the cross-route and mode interaction matrix.
  - See [design §10][design-10].
  - Requires tasks 5.2.1–5.2.3.
  - Cover shell/direct argv/PTY/native/MCP against safe mode, full-auto,
    approval-disabled mode, policy reload, stale approvals, incomplete
    analysis, and unavailable enforcement.
  - Success: deny/bypass-sensitive pairs and authority/replacement triples
    are exhaustive; remaining interactions have documented pairwise coverage.
- [ ] 5.2.5. Switch migrated routes from guarded mode to the common authority.
  - See [design §9][design-9].
  - Requires tasks 5.1.1–5.1.5 and 5.2.1–5.2.4.
  - Requires 4.2.4 for Bash routes.
  - Success: each route has no reachable legacy allow path and rollback selects
    a validated generation without restoring revoked approvals.

### 5.3. Retire duplicate mechanisms and close the migration

This step answers whether any legacy parser or policy evaluator can still
change a decision after all routes are migrated. Its result establishes whether
the consolidation is complete. See design §§9–12; RFC 0001 §§5–6, 9.

- [ ] 5.3.1. Remove competing decision caches and evaluators.
  - See [design §9][design-9].
  - Requires 5.2.5.
  - Retain only pure legacy matchers required by the time-bounded configuration
    reader; retire dead compatibility code separately from active guards.
  - Success: callsite reachability audit finds no old decision authority.
- [ ] 5.3.2. Remove Tree-sitter Bash fallbacks after consumer migration.
  - See [design §9][design-9].
  - Keep unrelated Tree-sitter use outside the change.
  - Success: supported Bash requests use Brush evidence; parse failure and
    unsupported shells remain explicitly incomplete.
- [ ] 5.3.3. Complete legacy configuration migration diagnostics.
  - See [design §9][design-9].
  - Report skipped malformed rules, broad learned approvals, changed defaults,
    and unsupported constructs before removing the reader.
  - Success: every compatibility entry is migrated, intentionally dropped,
    or given an owner and retirement boundary.
- [ ] 5.3.4. Release the unified policy authoring and operations guide.
  - See [design §11][design-11].
  - Document policy commands, schema version, source authority, explanation,
    activation failures, and sensitive-data boundaries.
  - Update relevant quick references, tool schemas, and runtime guidance.
  - See design §11 and RFC 0001 R7–R9.

## 6. Deferred extensions after the v1 boundary

Idea: if the first release has complete route mediation, explicit uncertainty,
and stable local policy activation, broader language and distribution support
can be evaluated without destabilizing the core authorization contract.

### 6.1. Evaluate extensions with separate evidence

These workstreams are described in the design but are not required for the
initial replacement. They need their own feasibility and authority decisions.

- [ ] 6.1.1. Evaluate additional shell dialects against the effect-graph
  contract.
  - See [design §12][design-12].
  - Requires phase 5; deferred work does not block the initial replacement.
  - Start only when the dialect has a fidelity corpus and bounded analyser.
  - See design §§4–5 and 12.
- [ ] 6.1.2. Evaluate signed remote policy distribution and archive import.
  - See [design §7.4][design-7-4].
  - Specify signature trust, rollback, revocation, and archive extraction
    isolation before implementing transport.
  - See design §7.4 and §12.
- [ ] 6.1.3. Evaluate richer executable models and optional Brush execution.
  - See [design §12][design-12].
  - Require per-model coverage evidence and GNU Bash compatibility results;
    do not promote models based on command names alone.
  - See design §§5.3 and 12.

[design-4]: ../design/diapason-shell-policy-design.md#4-shell-state-and-effect-graph-contract
[design-3]: ../design/diapason-shell-policy-design.md#3-architecture-and-ownership
[design-5]: ../design/diapason-shell-policy-design.md#5-brush-backed-bounded-analysis
[design-6]: ../design/diapason-shell-policy-design.md#6-regorus-contract-and-decision-algebra
[design-7]: ../design/diapason-shell-policy-design.md#7-drop-in-authoring-authority-and-activation
[design-7-4]: ../design/diapason-shell-policy-design.md#74-atomic-generation-lifecycle
[design-8]: ../design/diapason-shell-policy-design.md#8-approvals-caching-and-enforcement-binding
[design-8-2]: ../design/diapason-shell-policy-design.md#82-dependency-aware-caches
[design-8-3]: ../design/diapason-shell-policy-design.md#83-final-dispatch-guard
[design-9]: ../design/diapason-shell-policy-design.md#9-compatibility-and-retirement
[design-10]: ../design/diapason-shell-policy-design.md#10-verification-and-release-evidence
[design-11]: ../design/diapason-shell-policy-design.md#11-authoring-tools-and-operational-behaviour
[design-12]: ../design/diapason-shell-policy-design.md#12-decisions-risks-and-deferred-scope
