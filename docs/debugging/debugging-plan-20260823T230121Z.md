# Debugging Plan: Intermittent sub-agent planner failures

- **Generated**: 2026-08-23T23:01:21Z
- **Issue ID**: local investigation
- **Severity**: high
- **Falsification sub-agent**: alchemist
**Planning agent boundary**: This document was prepared by the planning agent.
Falsification must be executed by the named sub-agent, not by the planning
agent.

## Problem Statement

ACP sessions using Arli and `DeepSeek-V4-Flash-0731` intermittently fail to
start delegated wyvern and scrutineer agents with `parse planner response` or
`planner request failed`. A successful spawn should enter the delegated
agent's task loop. Instead, failed child archives contain no messages or
transcript entries, leaving the parent to repeat the work directly.

## Context Summary

- **First observed**: ACP sessions from 2026-08-23.
- **Reproduction rate**: intermittent.
- **Affected components**: child runner and planner harness.
- **Recent changes**: ACP sub-agents enabled.

Failures affected repeated wyvern attempts and an initial scrutineer attempt,
while another scrutineer succeeded in the same parent session. Child runners
inherit the normal full-auto harness configuration and use the Arli provider.

### Error Artefacts

```plaintext
wyvern: failed: parse planner response
wyvern retry: failed: planner request failed
scrutineer: failed: parse planner response
```

Failed child archives such as
`vtcode-zed-session-dd6c2e25-a236-4c4a-baf8-2359f30ef1e2-agent-scrutineer-20260823T204040579Z.json`
have `total_messages: 0`, an empty transcript, and nearly identical start and
end timestamps. By contrast, the successful child archive
`vtcode-zed-session-88446497-6a5f-49c3-91fb-5bd26910c5bf-agent-scrutineer-20260823T195422341Z.json`
contains 63 messages and 39 transcript entries.

### Information Gaps

- The bounded raw planner response is neither persisted in the child archive
  nor present in the corresponding debug log.
- `request_json_only` adds only generic anyhow context, so the provider error
  classification is not visible at the spawn boundary.
- CodeGraph semantic queries stalled during the investigation. The source
  trace therefore used exact error labels and symbol references.

______________________________________________________________________

## Hypotheses

### H1: Sub-agents inherit an unintended mandatory harness planner

**Claim**: Full-auto child runners inherit `PlanBuildEvaluate`; consequently,
every child makes a separate planner-model request before it receives the
delegated task, even though `set_subagent_mode(true)` has already been called.

**Plausibility**: High — `run_child_once` sets sub-agent mode only on the loop
detector, while `harness_plan_build_evaluate_enabled` does not inspect a
sub-agent-mode flag and `task_setup` calls `run_planner_phase` before entering
normal execution.

**Prediction**: With full-auto and the default harness mode, a child whose
first provider response is invalid planner JSON fails before recording any
normal conversation messages. With orchestration disabled, the same child
reaches its delegated task request.

#### H1 Falsification Plan

1. Use the injected provider seam to compare a sub-agent run under
   `PlanBuildEvaluate` with one under `Single`. Return invalid JSON for the
   first request. H1 is falsified if both configurations make the same planner
   request, or the `PlanBuildEvaluate` child reaches its normal task.
2. Inspect the provider call count and child archive before tool execution. H1
   is falsified if the first call is the delegated task, or normal messages are
   checkpointed before the parse error.

**Tooling**: Existing `AgentRunner` harness test doubles and a focused test or
temporary experiment only; no full repository gates.

**Confidence on falsification**: High. Provider call ordering and archive
contents directly identify whether the hidden planner is the failing boundary.

______________________________________________________________________

### H2: Arli returns syntactically or structurally invalid planner JSON

**Claim**: `parse planner response` denotes a successful provider call whose
content is empty, fenced in an unsupported form, prefixed/suffixed with prose,
or incompatible with `PlannerResponse`.

The OpenAI-chat adapter rejects non-success HTTP responses and HTTP 200 bodies
that are not valid response-envelope JSON. Those cases become
`planner request failed`. A provider error can reach this parser only if Arli
returns a valid Chat Completions envelope whose assistant content contains
plain-text error prose, or whose assistant content is empty.

**Plausibility**: High — `request_json_only` attaches this context only after
`generate` succeeds, and `parse_json_response` accepts only a bare JSON value
or a whole-response Markdown fence.

**Prediction**: Capturing a bounded, secret-safe summary of the failed content
will show either empty content, a JSON syntax error, surrounding prose, or a
Serde shape error.

#### H2 Falsification Plan

1. Exercise `parse_json_response::<PlannerResponse>` with bare, fenced,
   prose-wrapped, truncated, and wrong-shape DeepSeek responses. H2 is
   falsified if all realistic failure payloads parse successfully.
2. If safe instrumentation exists, capture only the payload length, hash,
   bounded first and last excerpts, and Serde error. H2 is falsified if the
   provider returned a valid `PlannerResponse` and failure arose after parsing.

**Tooling**: Focused parser tests and redacted diagnostic instrumentation in a
future fix; do not record full prompts or secrets.

**Confidence on falsification**: High for a captured live failure, medium for
synthetic fixtures alone.

______________________________________________________________________

### H3: Planner transport calls bypass ACP provider robustness

**Claim**: `request_json_only` calls `provider_client.generate` directly, so
planner requests do not use the ACP attempt policy, retry/backoff telemetry, or
separated streaming timeouts that protect ordinary ACP turns.

**Plausibility**: High — the direct call is visible in
`runner/orchestration.rs`, and the `planner request failed` context wraps that
call without an intervening retry layer.

**Prediction**: An injected transient provider failure aborts planning after a
single call and is surfaced only as `planner request failed`; an ordinary ACP
turn using the runtime policy retries the equivalent retryable failure.

#### H3 Falsification Plan

1. Supply a provider double that fails once with a retryable transport error
   and succeeds on its second call. H3 is falsified if planning retries and
   succeeds, or emits the same typed retry telemetry as the ACP runtime.

**Tooling**: Existing provider double and focused planner request test.

**Confidence on falsification**: High. Provider call count is decisive.

______________________________________________________________________

### H4: The role definitions or child tool policy are defective

**Claim**: Wyvern and scrutineer fail because their definitions, allowed tools,
or prompts cannot be loaded.

**Plausibility**: Low — a scrutineer child in the same parent session reached
63 messages, while failed child archives terminate before any messages.

**Prediction**: Definition or policy failures would be deterministic for a
given role and would occur before the provider planner labels are attached.

#### H4 Falsification Plan

1. Compare effective metadata and tool-policy construction for failed and
   successful scrutineer archives. H4 is falsified if the same role and
   configuration succeeds in one run and fails only at the planner boundary.

**Tooling**: Durable archive metadata and controller source trace.

**Confidence on falsification**: High. A successful same-role child strongly
rules out a static role-definition defect.

______________________________________________________________________

## Recommended Execution Order

1. **H1** — cheapest and most decisive; it explains why role execution never
   starts and identifies the architectural amplification.
2. **H3** — a focused provider call-count experiment separates the planner
   path from the robust ACP attempt path.
3. **H2** — parser fixtures are cheap, but a conclusive live diagnosis needs
   bounded diagnostic capture that does not yet exist.
4. **H4** — retain as a control; existing successful-child evidence already
   makes it unlikely.

## Falsification Results

H1 was **not falsified** with high confidence. The alchemist traced the child
runtime and existing injected-provider tests and confirmed this sequence:

```plaintext
inherit parent PlanBuildEvaluate configuration
enable full auto
enter execute_task
make non-streaming strict-JSON planner request
enter the delegated task only after planning succeeds
```

`set_subagent_mode(true)` changes only the loop detector. The child archive is
created before execution, while messages are persisted after `execute_task`
returns. This accounts for failed archives containing zero messages.

The evidence also separates the observed errors. `parse planner response`
means provider generation returned content that `parse_json_response` rejected.
`planner request failed` means the direct provider generation call failed. The
180.34-second failed wyvern attempt is consistent with a provider timeout; the
47-to-93-second parse failures returned earlier and then failed decoding.

## Termination Criteria

- **Root cause identified**: H1 survives call-order falsification and the two
  observed labels are explained independently by H2 and H3.
- **Escalation trigger**: A child configured with `Single` still invokes the
  planner, or a `PlanBuildEvaluate` child records normal task messages before
  producing either planner label.

## Notes for Executing Agent

Use only a focused deterministic experiment. Do not call Arli, mutate durable
user sessions, run full repository gates, or implement a fix. Report the
provider call sequence, the configuration difference, archive/message state,
and a verdict of falsified, not falsified, or inconclusive for H1.
