# Agent Capability as a Composed System

Agent capability is not a property of the model alone. As tasks become
long-horizon, capability is an emergent property of a *system* composed of
**model · harness · context · tools · evals · sandbox · state management**,
operating within **finite context**, reaching through **external tools**, into
a **changing environment** — and the only acceptable behaviour over time is that
the agent **keeps getting closer to completion**.

This document maps that framing onto concrete Diapason crates and modules, and
shows where each subsystem lives and how they reinforce the long-horizon
progress invariant. It complements [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md)
(the "Model + Harness" split) and
[`docs/context/context_engineering.md`](../context/context_engineering.md).

## The seven components

| Component            | Role in the system                                                                             | Where it lives                                                                                                                                                                                                                                  |
| -------------------- | ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Model**            | Reasoning engine.                                                                              | `diapason-llm/` (canonical provider trait, `providers/`, `cgp.rs`, `capabilities.rs`, `model_resolver.rs`); `crates/common/diapason-commons/src/model_family.rs`; `crates/codegen/diapason-core/src/llm/` re-export facade + `models_manager/`. |
| **Harness**          | The runtime that makes reasoning useful: instruction memory, continuation, hooks, guard rails. | `crates/codegen/diapason-core/src/core/agent/` + `src/agent/runloop/`; `crates/codegen/diapason-core/src/prompts/`; `continuation.rs` (`ContinuationController`).                                                                               |
| **Context**          | Finite attention budget, curated each turn.                                                    | `crates/codegen/diapason-core/src/context/`; `crates/codegen/diapason-core/src/compaction/` (`memory_envelope.rs`); `context.dynamic` spooling.                                                                                                 |
| **Tools**            | External capability surface.                                                                   | `crates/codegen/diapason-core/src/tools/`; `diapason-utility-tool-specs/`; `diapason-mcp/`; `diapason-skills/`.                                                                                                                                 |
| **Evals**            | Closing the loop on quality.                                                                   | `scripts/evals/` (Python `eval_engine.py`, `metrics.py`); `crates/codegen/diapason-core/src/llm/rl/` (`signal`/`ledger`/`engine`/`eval`: reward → RL).                                                                                          |
| **Sandbox**          | The controlled, changing environment.                                                          | `diapason-bash-runner/`; `diapason-safety/` (`command_safety`, `exec_policy`, `sandboxing`).                                                                                                                                                    |
| **State management** | Durable, resumable, cross-session.                                                             | `diapason-memory/`; `diapason-exec-events/` (`ThreadEvent`); `crates/codegen/diapason-core/src/loop_state.rs`, `persistent_memory`.                                                                                                             |

## The three environmental constraints

1. **Finite context.** The live window cannot hold everything. Diapason answers
   this with the *three context primitives*
   ([`context_engineering.md`](../context/context_engineering.md)):
   - **Memory** — durable facts outside the prompt (`/memories`,
     `SessionMemoryEnvelope`).
   - **Compaction** — shrink the whole transcript under token pressure
     (provider-native or local fallback with dedup).
   - **Tool-result offloading** — keep re-fetchable payloads out of the live
     window (split results, `output_spooler`, `context.dynamic`).
   Additionally, the durable **`ProgressLedger`**
   (`crates/codegen/diapason-memory/src/progress.rs`) is a tiny derived
   artefact that summarizes goal progress without reloading the event log.

2. **External tools.** The agent reaches beyond its weights through the tool
   registry, MCP, and skills. Tool policy / allow-lists (`tool_policy.rs`,
   `permissions.rs`) bound what the environment will permit.

3. **Changing environment.** The sandbox (`diapason-bash-runner`,
   `diapason-safety`) defines where code runs and what it can touch, and
   `crates/common/diapason-commons/src/workspace_snapshot.rs` provides cheap
   environment-delta detection (added/changed/removed files per turn) so the
   harness can re-ground stale assumptions.

## The progress invariant

> Over time, the agent must keep pushing toward the goal and get closer to
> completion.

Diapason enforces this at several layers:

- **Continuation controller** (`continuation.rs`) accepts completion only when
  the task tracker is complete *and* verification commands pass.
- **`ProgressMonitor`**
  (`crates/codegen/diapason-core/src/core/agent/progress_monitor.rs`) mirrors
  the tracker into a durable `ProgressLedger`, records advance/stall signals
  each turn, and checkpoints a human-readable summary to `memories/progress.md`
  (proactive context grounding, P3).
- **Stagnation / escalation** handling reroutes a stalled run toward
  compaction → replan → escalation.
- **RL action selection** (`crates/codegen/diapason-core/src/llm/rl/`) prefers
  low-latency, high-success actions and is fed by eval outcomes, so the
  *system* gets better at making progress, not just the model.
- **Budget guardrails** (`SessionBudget` in `usage_cost.rs`) pause or escalate
  long runs at cost thresholds so progress does not come at unbounded spend.

## How the pieces reinforce each other

```
        model ──► harness ──► tools ──► sandbox (changing env)
                     │            │
                context ◄───────┘   evals ──► rl (better actions)
                     │
                state (resume / progress ledger)
                     │
              progress invariant (monotonic completion)
```

A long task survives a crash because the `ProgressLedger` and `ThreadEvent` log
are on disk; it makes progress because the continuation controller refuses
premature completion; it adapts because eval outcomes feed the RL loop; it
stays bounded because budget guardrails exist. No single component delivers
long-horizon capability — the composition does.
