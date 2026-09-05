# Planning Workflow

The planning workflow lets you iterate with the agent on what you want to build before implementation starts. It is driven by the built-in `plan` primary agent and the `/plan` slash command. Plan is discussion-first: it remains a distinct read-only workflow that investigates the repository, uses evidence and reasonable defaults, and asks only when a material choice remains.

## Overview

During planning, the agent can:

- read files and inspect project structure
- search code with grep, structural search, and other read-only tools
- analyse patterns and constraints before proposing changes
- run explicitly safe inspection or validation commands when the active permission policy allows them
- ask clarifying questions through `request_user_input`

The planning agent does not implement changes, shell-write plan files, or use file-writing tools for plan persistence. It emits exactly one final `<proposed_plan>` block when the plan is ready. The runtime validates and persists that plan, then exposes approval controls. During an approved handoff, the runtime creates and persists the `task_tracker` before handing off to the write-capable `build` agent or configured `auto` workflow.

The built-in `plan` agent's permission rules allow `read`, `request_user_input`, and `bash` so its wire catalogue keeps `exec_command`, `code_search`, `grep_file`, and the interview tool visible. Read-only enforcement is not delegated to those permissions: the planning dispatch gate hard-blocks every mutating tool call (and non-allow-listed shell command) before execution, so granting `bash` admits the tool without weakening plan-mode safety. The `plan` agent is also excluded by name from approved-plan execution routing — selecting it always re-enters planning, never implementation.

## Bounded blocked-call recovery

Blocked and denied tool calls are bounded per turn to prevent retry churn. The configured `tools.max_consecutive_blocked_tool_calls_per_turn` value remains the consecutive-call cap. The total fuse is two times that cap in normal mode, four times that cap in Plan Mode, and the consecutive cap in recovery mode. The fuse is strict: with a cap of `3`, Plan Mode permits 12 non-consecutive blocked calls and stops on call 13. A successful or otherwise allowed call resets the consecutive streak, but not the turn's total blocked-call count.

When a turn stops because of blocked behaviour, VT Code forces a session-history checkpoint before writing the blocked handoff. The handoff advertises `vtcode --resume <archive-id>` only after that archive is successfully persisted and its identifier is verified. If history persistence is disabled or the checkpoint fails, the handoff explains that resume is unavailable and does not advertise a misleading command. Interactive sessions return to the next input after the handoff.

Runner paths that do not create session archives also omit the resume command and state that limitation in the handoff.

Shell commands in plan mode are validated against a read-only allow-list. Allowed patterns include:

- inspection base commands: `rg`, `ls`, `cat`, `sed`, `grep`, `find`, `head`, `tail`, `fd`, `tree`, `stat`, `file`, `which`, `jq`, and similar
- `cd` prefixes: `cd <dir> && <read-only command>` (changing directory mutates nothing)
- read-only subcommands: `git status|log|diff|show|blame|ls-files|rev-parse|describe|shortlog|grep`, `cargo check|test|clippy|metadata|tree|nextest run`, `npm|pnpm|yarn test`
- `&&` chains and `|` pipelines where every segment is itself read-only
- static `;` chains where every segment is independently read-only; literal-output `printf` is allowed as an inspection-output separator
- `2>&1` stderr merges (no file is written)

Rejected: file redirections (`>`, `>>`), command substitution (`$(...)`, backticks), dynamic `;` chains, in-place edits (`sed -i`), and any chain with a mutating or unknown segment (`rm`, `mv`, `cargo build`, `git push`, arbitrary scripts).

During planning, the dispatch gate denies mutating tools. Plan remains read-only; the runtime alone persists validated planning artefacts under `.vtcode/plans/`.

`task_tracker` is available for checklist state. Planning output should use `<proposed_plan>...</proposed_plan>` when the agent is ready for user review.

Tracker updates use action-aware indices. Standard checklist item indices are
positive and 1-based; the compatibility form `index: 0` is reserved for
checklist-level completion with `status: "completed"`. Planning workflow
updates accept positive flat indices or positive hierarchical `index_path`
values such as `2.1`. Use `items` for bulk synchronization rather than an
item index.

Successful tracker updates render as one compact hierarchical tree in both the
inline transcript and the TODO panel. Parent rows show their branch and
description; leaf rows use `[-]`, `□`, `[x]`, or `[!]` for in-progress, pending,
completed, and blocked work. Files, outcomes, and verification commands remain
structured tracker metadata rather than extra visible rows.

## Usage

### Start With The Planning Agent

Set the default primary agent to `plan` when you want new sessions to start with the built-in planning agent:

```toml
default_primary_agent = "plan"
```

You can also press `Tab` on an empty idle composer to cycle to the `plan` primary agent.

### Use `/plan`

`/plan` starts or continues the planning workflow. It is a workflow command, not a session state selector.

While a turn is actively processing, `/plan` is dropped with a notice (mode switches are locked for the duration of a turn). The automatic in-turn planning intent detection still engages on its own; only explicit `/plan` entry while busy is deferred.

```text
/plan
```

### Enter From an Agent Suggestion

The agent can also propose entering the planning workflow on its own when it
judges edits should be planned first. Under an interactive policy a HITL
confirmation prompt appears:

```text
Enter Planning workflow?

- Enter Planning workflow (Recommended) — enter read-only planning and research
- Continue without Planning workflow
```

- **Enter Planning workflow** — starts planning; read-only research begins and
  mutating tools stay disabled until you approve execution. The runtime persists
  the validated plan after the final `<proposed_plan>` is emitted.
- **Continue without Planning workflow** — the agent proceeds without planning
  (mutating tools remain enabled).

This gate prevents the agent from silently switching into plan mode; you decide
whether to plan before any edits begin. Full-auto and skip-confirmations
policies accept the suggestion directly, including in an interactive UI.

Execution agents such as `build`, `auto`, and `duck` can invoke the
`start_planning` tool when a request is demanding, ambiguous, or has multiple
phases. The tool only presents the entry prompt; it does not silently change
mode. Straightforward requests continue directly in the active execution
agent. In a headless session without an automatic execution policy, the
suggestion is reported as pending and the turn stops safely; use `/plan` on the
next turn to confirm entry. Full-auto or skip-confirmations policies may accept
the suggestion automatically.

### Intent Phrases

You can steer the workflow with short phrases instead of the review-gate UI:

- To **exit planning and present the plan** for approval, type `implement`,
  `approve`, `lgtm`, `ship it`, `yes`/`continue`/`go`/`start`, or select
  **Execute** / **Auto-accept** in the review gate. The plan is shown in an
  inline confirmation overlay (or a text prompt in non-interactive mode);
  the agent will not self-approve by editing the plan file and staying in
  plan mode.
- To **stay in planning**, type `stay in planning` (or revise the
  `<proposed_plan>` block). This overrides any exit phrase.
- To **cancel planning without implementation**, type `no`, `cancel`, or
  `abandon plan`, use `/plan off`, or switch away from the `plan` agent. VT
  Code emits a terminal `cancel` approval event and removes the active plan
  draft when the workflow is explicitly abandoned.

### Typical Workflow

1. Select the `plan` primary agent or run `/plan`.
2. Describe the goal and constraints.
3. Iterate on repository facts, risks, and open decisions.
4. Review the emitted `<proposed_plan>` block.
5. Switch to a build-oriented primary agent such as `build` or `auto` when you are ready to implement.

When planning was entered from another primary agent, approving the plan
restores that agent automatically when it is write-capable. `build` resumes
with reviewable edits and `auto` resumes its configured automation policy.
Read-only agents such as `duck` and `plan` are never selected to execute an
approved plan; the handoff resolves to the configured write-capable agent or
the built-in `build` agent. If the dedicated `plan` agent was already active
when planning began, approval uses the configured default execution agent
only when that agent can mutate the workspace.

The approval overlay shows a compact synopsis so its choices remain visible,
while the complete plan markdown is appended to the scrollable TUI transcript
and remains available from the persisted plan file. The queued implementation
turn revalidates the handoff, exits any stale planning gate, and refreshes the
selected agent's permission snapshot before tools are exposed. Mouse-wheel
events outside the floating overlay are passed through to the transcript, so
the full plan can be reviewed without dismissing the approval gate. Approval
also distils the plan's numbered and checkbox steps into the `task_tracker`
checklist; the implementation agent updates that checklist as work progresses.
Approved-plan execution receives a separate implementation safety budget so
planning research does not leave the build phase with only the ordinary
short-turn allowance.

That implementation turn also receives one internal `+50` tool-loop allowance
at initialization. The allowance is clamped by the ordinary loop hard cap
(for example, the default `40` becomes `90`, while `100` remains capped at
`120`), does not change the tool-call budget, and does not stack with manual
loop extensions. A configured loop value of `0` remains unlimited. Current and
fresh-context approved-plan handoffs each initialize their own implementation
turn, so each gets the same one-time allowance.

The implementation request is scheduled as an explicit internal next-turn
trigger after the handoff directive is appended. It is not dependent on the
bounded ordinary steering FIFO, and the synthetic user prompt is recorded once
so an approved plan cannot switch to `build` and then wait for another
`continue` input.

### Validated Approval Handoff

Approval is accepted only for a persisted plan that passes the artefact validator and has a persisted task tracker. The canonical sections are `Summary`, `Implementation Steps`, `Test Cases and Validation`, and `Assumptions and Defaults`; the documented short aliases `Steps`, `Validation`, and `Assumptions` are accepted case-insensitively. Plans may also include optional `Expected Outcomes` and `Dependencies and Prerequisites` sections; the validator tolerates additional sections and enforces only the canonical four. Every numbered implementation step must name a concrete file, symbol, behaviour, or other repository target and include a non-empty `verify:`/`verification:` command or check. Placeholder tokens and unresolved `Next open decision` or `Open question` entries block approval. Invalid candidates are rejected before persistence, so an existing valid draft is preserved. VT Code gives the model one bounded repair request; if the repaired artefact is still invalid, planning remains active with the validation reasons visible.

Creating the `task_tracker` checklist is part of the approval gate. If the tracker tool is unavailable, fails, or does not persist its tracker file, the planning workflow remains active and no write-capable execution turn is started. All approval routes share the same typed handoff, including direct, queued, automatic, and fresh-context execution.

### Streaming-to-persistence handoff

In streaming mode, `<proposed_plan>` markup is removed from the live transcript
so the plan is rendered by the approval flow exactly once. The stream parser
retains the plan body, including when tag boundaries are split across tokens,
and attaches one canonical `<proposed_plan>...</proposed_plan>` block to the
completed response before response processing runs. Completed provider content
takes precedence over visible streamed prose; an existing recognized plan block
is not duplicated. This semantic handoff also occurs when output is suppressed
while verification is pending, so the normal response-processing path still
extracts, validates, persists, and publishes approval events. Rendering
suppression never bypasses plan validation or persistence.

### Clarification Interviews

When the planning agent reaches a material ambiguity, or identifies an open
decision before presenting a plan, it asks through `request_user_input`. In an
interactive session this opens an inline wizard with a selectable option list
(and an optional free-form note). The selected answer is returned to the same
planning turn, recorded as the completed interview, and supplied to the next
model request so planning can continue with the user's choice.

Pressing `Esc` or `Ctrl-C` cancels the interview without submitting an answer;
the planning agent may ask again when the ambiguity still matters. A plan
approval popup is not shown until any required clarification has completed.

`request_user_input` is optional in headless and other noninteractive runtimes.
A permanent denial is recorded for the planning session and suppresses repeated
interview attempts. VT Code gives the model one bounded synthesis retry using
the repository evidence already gathered. If that retry does not produce a
validated persisted plan, the session stays in planning and shows a keep-planning
message; it does not advertise implementation or emit approval events.

### Empty-response recovery

Two consecutive empty model responses in planning have one deterministic
recovery path. The first empty response receives the ordinary tool-enabled
retry. The second schedules exactly one tool-free synthesis using the latest
request and bounded recent evidence; the synthesis must contain exactly one
canonical `<proposed_plan>...</proposed_plan>` block and no tools, questions, or
approval prose. The runtime validates and persists the block before exposing
approval controls.

If synthesis is empty, malformed, contains tool markup, fails validation, or
cannot be persisted, the runtime preserves any rejected draft, emits a
concise actionable blocked handoff, and keeps planning active. It does not
inject another interactive question, claim completion, or emit
`thread.completed`; the blocked turn is resumable and the session is finalized
only during shutdown.

## Plan Output Format

Planning output should stay decision-complete but sparse — treat it like a
compact spec, not prose. Keep the whole `<proposed_plan>` under ~1500 tokens;
prefer `file:symbol` references over narrative. This bound exists because an
overly verbose plan is truncated at the model's output-token limit (cut off
mid-plan) and must then be condensed and re-emitted.

File references must be plain text or inline code, never markdown links or
editor/IDE URIs — plans are read in terminals and other non-hyperlink
surfaces:

```markdown
Correct: `src/main.rs:42` or src/main.rs:42
Incorrect: [main.rs:42](vscode-file://vscode-app/.../workbench.html)
Incorrect: [main.rs](file:///Users/you/repo/src/main.rs#L42)
```

```markdown
Repository facts checked:

- [file:symbol or behaviour confirmed from the repo]
- [observed command output -> the insight it establishes]

Next open decision: [if any], otherwise: No remaining scope decisions.

<proposed_plan>

# [Task Title]

## Summary

[1-3 lines: goal, user impact, what changes / what does not]

## Implementation Steps

1. [Action] -> files: [path/to/file.rs] -> verify: [check]
2. [Action] -> files: [path/to/file.rs] -> verify: [check]

## Test Cases and Validation

- build/lint: [detected toolchain command]
- tests: [detected toolchain command]
- behaviour: [targeted check]

## Expected Outcomes

- [observable end state the implementation must produce]

## Dependencies and Prerequisites

- [required tooling, configuration, or prior work]

## Assumptions and Defaults

- [assumption or default chosen]
- [out-of-scope item intentionally not changed]

</proposed_plan>
```

`Expected Outcomes` and `Dependencies and Prerequisites` are optional sections:
add them when they are material to the request — what observable end state the
work must produce, and which tooling, configuration, or prior work must exist
before implementation — and omit them when nothing material exists. The
validator tolerates additional sections; only `Summary`, `Implementation Steps`,
`Test Cases and Validation`, and `Assumptions and Defaults` are required.

`Next open decision` and `Open question` entries are explicit reopen markers for follow-up planning; use a resolved statement such as `No remaining scope decisions` when none remain.

### Reasoning and Evidence

A plan is only as strong as the evidence behind it, and planning output should
show the reasoning chain, not just the conclusion:

- Ground every claim in a `file:symbol` reference or an observed read-only
  command output. Never speculate about code you have not opened.
- Pair each observed fact with its insight. A failing test line is evidence;
  the root cause, constraint, or bottleneck it establishes is the insight.
  Record the insight — the raw output belongs in the transcript, not the plan.
- Put load-bearing findings (a verified root cause, a hard constraint) in
  `Repository facts checked` or, when they define the work itself, in
  `Summary`.
- Make each step's `verify:` check the stated insight, so approval hands the
  implementation agent a falsifiable target instead of a description.

### Common Scenarios

The reasoning chain above applies whenever a request needs diagnosis before
change. Three recurring cases:

**Troubleshooting a command failure.** A build, test, or runtime command fails.
Run it read-only, capture the decisive output lines, and isolate the offending
code before proposing a fix:

```markdown
Repository facts checked:

- `cargo test parser::tests::parse_case` fails: `assertion failed: left == right` at src/parser.rs:120
- src/parser.rs:118 advances the cursor before the bounds check, so the final byte is read twice

<proposed_plan>

# Fix off-by-one in parser cursor advance

## Summary

`parse_case` reads the final byte twice and fails the round-trip test. The fix
hoists the bounds check above the cursor advance.

## Implementation Steps

1. Hoist bounds check above cursor advance -> files: [src/parser.rs] -> verify: [cargo test parser::tests::parse_case]
2. Add single-byte regression test -> files: [src/parser.rs] -> verify: [cargo test parser::tests::single_byte]

## Test Cases and Validation

- build/lint: `cargo check`; `cargo clippy --workspace --all-targets -- -D warnings`
- tests: `cargo nextest run`
- behaviour: the previously failing `parse_case` passes

## Expected Outcomes

- `parse_case` accepts single-byte input without double-reading; no other parser behaviour changes

## Assumptions and Defaults

- only the cursor advance changes; the grammar itself is out of scope

</proposed_plan>
```

**Optimizing a workflow.** A slow build, test, or check loop. Measure first so
the numbers drive the plan, then verify the win without breaking correctness:

- Repository facts: `cargo build --timings` puts 41% of build time in one
  crate; the check script rebuilds three crates the dev script does not.
- Insight: the bottleneck is that crate's feature set, not link time.
- Steps shape: trim the unused feature → re-time the build against the
  recorded baseline → run the full test gate. Each `verify:` is either the
  timing comparison or the correctness gate, so the optimization cannot ship
  as an unmeasured claim.

**Root-cause bug fix.** For a behavioural bug — for example, a fullscreen TUI
transcript leaking into the CLI scrollback after exit — the diagnosis-heavy
plan maps onto the template section by section:

| Diagnosis plan part | Template home |
| ------------------- | ------------- |
| Root cause (verified in code) | `Repository facts checked` — file:line evidence for the shutdown race |
| Fix, in layers | `Implementation Steps`, each with its own `verify:` |
| Verification and docs work | `Test Cases and Validation` |
| Scope notes | `Assumptions and Defaults` (intentionally unchanged surface) |
| — | `Expected Outcomes`: "CLI scrollback is clean after a plain exit" |
| — | `Dependencies and Prerequisites`: PTY harness available for the regression check |

### Research Scope

Research effort should scale with the request. The runtime gives planning a
minimum per-turn tool-call budget of 120 and a minimum loop budget of 60 when
configured nonzero limits are lower; these floors are planning-specific and do
not change the conversation-turn retention limit. For a narrow or simple ask,
a handful of targeted reads/searches (roughly 5-10) is usually enough before
drafting `<proposed_plan>` — exhaustively enumerating the whole repository
for a simple request wastes the turn's tool-call and wall-clock budget and
can exhaust it before a plan is produced. For a broad or ambiguous ask,
research proportionally more, but stop and draft as soon as the
scope/decomposition/verification decisions are closed.

The 120-call ceiling remains available for complex work; it is not a target.
Planning also tracks low-signal navigation separately from the hard ceiling.
Six consecutive low-signal reads/searches, or ten total within one turn,
schedule one tool-free synthesis pass from the evidence already gathered. A
productive navigation resets only the consecutive count. Verification,
mutation, recovery, and a new turn reset both counts. Shell inspection such as
`rg`, `find`, `cat`, and simple `sed -n` contributes to navigation accounting;
compile, test, build, and clippy commands count as verification progress.

## Review Gate

After a plan is ready, an interactive human-in-the-loop (HITL) confirmation popup presents a bounded, decision-ready synopsis
(summary plus numbered steps) and a decision gate. The complete markdown remains available in the persisted plan file and
runtime plan events; long previews are elided with an explicit count rather than silently clipped.

Approval options:

- **Yes, implement this plan** — execute in the current context while preserving the session's existing confirmation policy.
- **Yes, clear context and implement** — preserve the approved plan and task tracker, then rebuild a fresh execution thread. The subtitle reports the pre-reset context usage (for example, `Fresh thread. Context: 7% used.`). This is recommended after long research sessions.
- **No, stay in Plan mode** — return to planning and revise the plan.

The existing manual or auto-accept policy selected by the session remains attached to both
approval paths. A fresh handoff clears only transient transcript, continuation, cache-lineage,
recovery, and tool-budget state; the plan file, task tracker, working tree, configuration,
provider, permissions, and aggregate usage remain intact. The UI shows `Preparing fresh execution
thread...`, `Restoring approved plan...`, and `Starting build...` while the handoff is active and
guards input and mode switches until it completes.

The confirmation policy is explicit handoff state and is not inferred from the destination
agent's name. Textual approvals use the same policy already selected by the session.

The approved execution turn ends with a concise summary of the outcome, changed
files, verification performed, and remaining blockers. In interactive sessions
this appears alongside the final response. In headless sessions, a plan that
requires confirmation remains pending and the run stops safely after emitting
the plan; send `approve` or `implement` on a later turn to continue. Existing
full-auto or skip-confirmation policies may continue automatically without an
interactive prompt.

### Runtime Events

All clients can reconstruct the approval lifecycle from the authoritative
`ThreadEvent` stream. A plan turn emits `plan.delta` and the completed plan item,
then `plan.approval.requested` with the producing turn and plan file. The
terminal decision is emitted as `plan.approval.resolved` with one of
`execute`, `fresh_context`, `revise`, `cancel`, or a legacy handoff decision,
plus an `automatic` flag. A successful fresh handoff then emits `context.reset`
with the trigger, plan-preserved status, previous context usage, and tool-budget
reset status. The Open Responses bridge forwards this as `vtcode.context_reset`.

## Budget Exhaustion

If the configured planning tool-loop limit is reached, the runloop stops
research and enters the tool-free recovery path. It asks for one compact
decision-ready plan from the evidence already gathered rather than ending with
only a loop-limit message. After synthesis, review the plan and choose the
current-context implementation or the fresh-thread handoff. Session budget and
wall-clock hard caps remain enforced; when those caps prevent synthesis, the
draft and research are preserved for a later approval or revision command.

During implementation, tool results remain causally attached to the assistant
tool-call batch that produced them. Recovery and system directives are appended
only after the complete batch, so a provider never receives a directive between
the assistant call and its results. Request assembly may create a repaired,
request-only view when older history contains split, orphaned, duplicate, or
missing results; the durable session history is not rewritten by this repair.

## Plan File Persistence

The runtime-owned draft is the single source of truth and lives on disk under
`.vtcode/plans/<plan>.md`, not only in chat history. The planning agent never
writes this file itself. A candidate is validated
before the plan file, sidecar tracker, global tracker, or approval events are
created or updated. Invalid or partial inline plans from normal or tool-free
recovery are discarded for approval and cannot overwrite an existing valid
draft; the existing malformed file is preserved until a later valid synthesis
repairs it. Only a validated `<proposed_plan>` is extracted and written during
tool-free recovery. If no valid draft exists, the recovery message tells the
user to keep planning rather than offering `implement`.

## Best Practices

1. Be specific about files, functions, constraints, and desired behaviour.
2. Ask the agent to state trade-offs before implementation begins.
3. Ask the agent to record expected outcomes and any prerequisites when they affect implementation order or risk.
4. Keep the planning agent read-oriented and switch to `build`, `auto`, or `review` for the next phase.

## See Also

- [Command reference](../user-guide/commands.md)
- [Subagents and primary agents](../user-guide/subagents.md)
- [Configuration precedence](../config/CONFIGURATION_PRECEDENCE.md)
