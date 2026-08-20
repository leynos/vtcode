# Debugging Plan: ACP omits skill and sub-agent catalogues

**Generated**: 2026-08-20

**Issue ID**: local investigation on `investigate/acp-skill-agent-discovery`

**Severity**: medium

**Falsification sub-agent**: `alchemist`

**Planning agent boundary**: This document was prepared by the planning agent.
Falsification must be executed by the named sub-agent, not by the planning
agent.

## Problem Statement

ACP agents receive neither the model-visible skill catalogue discovered from
`~/.codex/skills` nor a catalogue of the named sub-agents discovered from
`~/.claude/agents`. The expected behaviour is that the system prompt names
eligible skills and that the collaboration tool tells the model which concrete
agent types it may select. The missing routing context makes agents rediscover
files manually or depend on names repeated in user instructions.

## Context Summary

| Aspect              | Details                                 |
| ------------------- | --------------------------------------- |
| First observed      | User report, 2026-08-20                 |
| Reproduction rate   | Latest seven ordinary archives: 7/7     |
| Affected components | ACP prompt and collaboration projection |
| Recent changes      | PR #13 did not change prompt discovery  |

### Error Artefacts

```plaintext
Recent archive system messages: no "Available Skills", SKILL.md routing, or
alchemist/scribe/scrutineer/wyvern catalogue.

Older ACP archives and debug logs: agent calls for wyvern, scribe, and
scrutineer execute successfully when their names are supplied elsewhere.
```

### Information Gaps

The durable archive does not preserve the provider tool-definition payload, so
archive inspection alone cannot prove whether an unused `agent` tool was on a
specific request. A deterministic construction-path experiment must inspect
the generated prompt and tool schema directly.

______________________________________________________________________

## Hypotheses

### H1: ACP bypasses populated prompt context

**Claim**: `run_acp_agent` calls the compatibility system-prompt builder, which
always composes with `prompt_context = None`; therefore skill discovery and the
skills section cannot run for ACP.

**Plausibility**: high — the ordinary agent runner explicitly calls
`PromptContext::load_available_skills_async`, while ACP calls
`generate_system_instruction_with_config`, whose implementation passes no
prompt context.

**Prediction**: If this hypothesis holds, an eligible skill placed in an
isolated Codex home appears when composing with a loaded `PromptContext`, but
does not appear through ACP's compatibility builder for the same workspace and
configuration.

#### H1 Falsification Plan

| Step | Action                  | Expected negative result     |
| ---- | ----------------------- | ---------------------------- |
| 1    | Exercise isolated skill | ACP output names the skill   |
| 2    | Compare call sites      | ACP passes populated context |

**Tooling**: Leta and one focused `cargo nextest` test or the smallest existing
prompt test seam; no full repository gate.

**Confidence on falsification**: high. The named marker either crosses the
exact ACP prompt boundary or it does not.

______________________________________________________________________

### H2: The collaboration schema omits discovered agent identities

**Claim**: ACP successfully installs a `SubagentController`, but the projected
`agent` tool schema declares `agent_type` as an unconstrained string and no ACP
prompt section renders the controller's discovered names or descriptions.

**Plausibility**: high — discovery includes `~/.claude/agents`, historical ACP
logs show successful named child execution, and `agent_parameters()` contains
no enum or catalogue metadata.

**Prediction**: If this hypothesis holds, discovery returns the four installed
Claude agents, while the exact ACP-visible `agent` definition contains none of
their names or descriptions.

#### H2 Falsification Plan

| Step | Action                     | Expected negative result |
| ---- | -------------------------- | ------------------------ |
| 1    | Inspect discovered names   | Claude agents are absent |
| 2    | Inspect ACP `agent` schema | Schema names every agent |

**Tooling**: focused config/core or ACP tests and schema inspection; no provider
or network request.

**Confidence on falsification**: high. Discovery and presentation are separate
deterministic values.

______________________________________________________________________

### H3: Provider concurrency disables the ACP controller

**Claim**: Arli's request cap leaves no child-provider permit, so ACP suppresses
the collaboration tool before requests reach the model.

**Plausibility**: low — current logs say concurrency is capped from three to
two, not disabled, and show successful ACP child execution.

**Prediction**: If this hypothesis holds, startup logs report controller
disablement or the ACP tool catalogue lacks `agent` even when sub-agents are
enabled.

#### H3 Falsification Plan

| Step | Action                 | Expected negative result   |
| ---- | ---------------------- | -------------------------- |
| 1    | Check logs and catalog | Controller and tool remain |

**Tooling**: existing logs and focused nextest filter.

**Confidence on falsification**: high for the observed configuration.

______________________________________________________________________

## Recommended Execution Order

1. **H3** — cheapest control; distinguishes missing capability from missing
   catalogue.
2. **H1** — exact skill prompt boundary and most likely root cause.
3. **H2** — exact discovery-versus-presentation boundary for sub-agents.

## Termination Criteria

- **Root cause identified**: H1 or H2 survives its direct construction-path
  experiment while H3 is falsified.
- **Escalation trigger**: all three hypotheses are falsified, or the ACP client
  mutates provider prompt/tool payloads after VTCode constructs them.

## Falsification Results

### H1: Not falsified

The ACP entry points in `crates/codegen/vtcode-acp/src/zed/session.rs` and
`crates/codegen/vtcode-acp/src/zed/agent/mod.rs` call
`generate_system_instruction_with_config`. That compatibility path calls
`compose_system_instruction_with_report` with `prompt_context = None`. The
ordinary runner instead loads available skills into `PromptContext` and passes
it to the composer. The focused skills-section test passed, proving that the
renderer emits the catalogue when context is present.

Evidence command:

```plaintext
cargo nextest run -p vtcode-core \
  test_skills_section_stays_lean_and_routing_focused
```

Result: one passed, 3,337 skipped.

### H2: Not falsified

`discover_subagents` includes `$HOME/.claude/agents`; the installed files are
`alchemist.md`, `scribe.md`, `scrutineer.md`, and `wyvern.md`. ACP successfully
attaches the controller and exposes the generic `agent` tool. Its shared static
schema, however, defines `agent_type` only as an unconstrained string. ACP also
bypasses the ordinary runner's budgeted sub-agent prompt section, so neither
presentation surface names the discovered choices.

Evidence command:

```plaintext
cargo nextest run -p vtcode-acp \
  -E 'test(enabled_subagents_are_exposed_to_acp_build_agents)'
```

Result: one passed, 186 skipped.

### H3: Falsified

Current debug logs report that Arli's `max_in_flight = 3` reduces configured
ACP child concurrency from three to two. They do not report controller
disablement. The same focused ACP test proves that the controller is attached
and the `agent` tool remains in the model catalogue. The log transcript for the
focused experiment is
`/tmp/acp-h3-nextest-investigate-acp-skill-agent-discovery.out`.

## Root Cause and Recommended Repair Boundary

This is an ACP composition divergence, not a filesystem-discovery or provider
capacity failure. The non-ACP unified runner already has the desired behaviour:
it loads skill metadata into `PromptContext`, derives a filtered list from
`SubagentController::effective_specs`, and appends a budget-aware catalogue of
sub-agent names, descriptions, and read/write posture.

The repair should make that prompt assembly reusable below the binary crate and
call it from both ordinary and ACP session setup. ACP should:

1. load skill metadata through `PromptContext::load_available_skills_async`;
2. derive model-visible sub-agents from the attached controller's effective
   specs, filtering out primary-only definitions;
3. use the same full, summarized, and token-budgeted sub-agent rendering as the
   unified runner; and
4. add an ACP construction regression proving an isolated Codex skill and
   isolated Claude agent both reach the provider-facing system prompt.

Keeping the generic `agent_type` schema is compatible with the existing
unified runner if the prompt catalogue is restored. Enriching it with dynamic
enum metadata is a possible follow-up, but duplicating the catalogue in both
the schema and prompt would create two independently budgeted sources of truth.
The shared prompt route is therefore the smaller and more congruent first fix.

## Notes for Executing Agent

Work read-only unless a test seam is strictly necessary. Do not edit production
code, run full repository gates, inspect credentials, or include user prompt
bodies in the report. Report exact symbols, focused commands, and whether each
hypothesis is falsified, not falsified, or inconclusive.
