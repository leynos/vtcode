# Debugging Plan: ACP context admission allows an over-limit Arli request

- **Generated**: 2026-08-21T18:42:22+02:00
- **Issue ID**: local ACP context-limit regression
- **Severity**: High
- **Falsification sub-agent**: alchemist
**Planning agent boundary**: This document was prepared by the planning agent.
Falsification must be executed by the named sub-agent, not by the planning
agent.

## Problem Statement

VTCode's ACP path is expected to compact or reject a request before the
provider's context window is exceeded. The currently installed build instead
sent Arli a request containing at least 507,905 input tokens while reserving
16,384 output tokens against a 524,288-token window. Arli rejected the request
because the combined total was 524,289. Repeated user attempts reached the same
failure, so session continuation was ineffective.

## Context Summary

- **First observed**: 2026-08-20T19:30:07Z in the retained ACP debug log.
- **Reproduction rate**: six consecutive prompts in one durable session.
- **Affected components**: `vtcode-acp` preflight compaction and the Arli
  custom OpenAI-chat provider.
- **Recent changes**: ACP compaction was wired before initial and post-tool
  provider calls.

### Error Artefacts

```plaintext
This model's maximum context length is 524288 tokens. However, you requested
16384 output tokens and your prompt contains at least 507905 input tokens, for
a total of at least 524289 tokens.
```

The corresponding durable archive contains 717 messages and 1,488,729 content
characters. The configured Arli profile declares `context_window = 524288`.

### Information Gaps

Arli does not expose its exact DeepSeek tokenizer locally. The durable archive
does not contain the fully resolved request (notably the current system prompt
and tool definitions), and the existing debug log does not record the local
preflight token estimate or admission budget.

### Progress

- H2 was falsified with high confidence. The production custom-provider router
  resolves the exact configured model profile and returns its 524,288-token
  context window before any backend fallback.
- Source inspection falsifies the main H3 call-path concern: production ACP
  constructs `ZedAgent` with `Some(&vt_cfg_clone)`, and all initial,
  post-tool streaming, and non-streaming provider dispatches call
  `maybe_compact_session` first.
- The failed run contains no successful-compaction, no-op-compaction, or safe
  admission-budget log record. It therefore passed the local estimate check.
- The archive contains about 403,955 characters of system feedback, chiefly 45
  repeated stop-hook reports, and 912,732 characters of tool-result content.
  This composition is a plausible source of provider-tokenizer divergence.
- H1 remains not falsified, with medium confidence. A temporary offline probe
  using production `SessionMessage` conversion and `Message::estimate_tokens`
  counted the 717 archived messages at 464,165 tokens: 6,671 below ACP's
  470,836-token admission budget, but 43,740 below Arli's reported minimum
  input count. The archive omits the generated system prompt and current tool
  definitions, so exact preflight telemetry is still needed for proof.

______________________________________________________________________

## Hypotheses

### H1: The generic tokenizer materially undercounts the DeepSeek prompt

**Claim**: ACP admission counts messages with `cl100k_base`, while Arli counts
the request with the DeepSeek model tokenizer; for this code-heavy history the
local estimate remains below the 470,836-token admission budget even though
Arli counts at least 507,905 input tokens.

**Plausibility**: High — the archive contains 1.49 million content characters,
the estimator explicitly documents itself as approximate and provider-agnostic,
and the configured context window is correct.

**Prediction**: Reconstructing the closest locally available request will yield
a local estimate below 470,836, despite Arli's recorded count of 507,905.

#### H1 Falsification Plan

1. Use the production token estimator on the archived messages and current ACP
   tool definitions. A local estimate at or above 470,836 disproves estimator
   undercount as the admission escape.
2. Compare message-only and tool-definition contributions. A message-only
   estimate above the budget rules out omitted tools as an explanation.

**Tooling**: One narrowly scoped test or existing diagnostic seam; no network
request and no full repository gate.

**Confidence on falsification**: High if the request reconstruction includes
the resolved system prompt and the same tool catalogue.

______________________________________________________________________

### H2: ACP receives the wrong effective context window

**Claim**: The runtime custom-provider router does not propagate the configured
524,288-token window to the provider used by ACP, so its admission budget is
computed from a larger fallback window.

**Plausibility**: Medium — the custom provider profile and router are separate
layers, although source inspection shows the router prefers the profile's
configured context window.

**Prediction**: The exact Arli provider constructed from the active config will
report an effective context size other than 524,288.

#### H2 Falsification Plan

1. Construct the Arli provider from `/home/leynos/.vtcode/vtcode.toml` without
   sending a request, then query `effective_context_size`. A result of 524,288
   falsifies H2.

**Tooling**: Existing provider factory and a focused test or diagnostic binary.

**Confidence on falsification**: Decisive.

______________________________________________________________________

### H3: ACP bypasses the compaction preflight on this call path

**Claim**: The failed provider call follows a branch that does not execute
`maybe_compact_session`, or the ACP agent has no `VTCodeConfig` and returns
early from that function.

**Plausibility**: Low — current source calls the guard before the initial call
and after every tool result, but the log lacks estimate/guard telemetry.

**Prediction**: A focused ACP lifecycle test will reach provider dispatch
without invoking the compaction seam.

#### H3 Falsification Plan

1. Exercise initial and post-tool ACP dispatch with an instrumented compaction
   seam. Observing the guard before every dispatch falsifies H3.

**Tooling**: Existing injected ACP provider test harness.

**Confidence on falsification**: High for the tested call paths.

______________________________________________________________________

## Recommended Execution Order

1. **H2** — cheapest and decisive; validates the configured hard limit.
2. **H1** — most plausible and directly measures the suspected mismatch.
3. **H3** — only if H1 is falsified or reconstructed estimates conflict with
   the live provider rejection.

## Termination Criteria

- **Root cause identified**: One hypothesis survives its decisive experiment
  and explains why a request passed preflight but failed provider admission.
- **Escalation trigger**: If the exact provider reports 524,288 and the local
  reconstructed estimate is at least 470,836, add preflight telemetry and
  capture the next failing request rather than inferring from the archive.

## Current Conclusion

The guard exists at every production ACP provider-dispatch site and receives
the correct provider context window, but it is not a hard context-limit
guarantee because admission is based on an approximate, provider-agnostic
token estimate. The current live failure passed that estimate and was rejected
by Arli. The implementation is therefore correctly wired but behaviourally
insufficient. Exact component telemetry is the next decisive diagnostic;
provider-specific counting or a conservative calibrated admission margin is
required for a reliable fix.

## Notes for Executing Agent

Do not contact Arli, expose credentials, modify runtime code, or run full
repository gates. Use the shared Cargo cache and wait naturally for its lock.
The live evidence is in
`/home/leynos/.vtcode/logs/debug-cmd-acp-1787246762148-2750883.log` and
`/home/leynos/.config/vtcode/sessions/vtcode-zed-session-3f0b6b54-e2b2-47f1-b746-3d066a2d7ace.json`.
