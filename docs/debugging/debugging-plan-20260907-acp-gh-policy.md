# Debugging plan: ACP denies authorized GitHub CLI commands

Generated: 7 September 2026. Priority: immediate user-reported interruption.
Falsification sub-agent: alchemist. Root is the planning agent and does not
execute the falsification experiment.

## Problem statement

The ACP session ae7940cf-5804-4edf-9e75-cbaba123b74b cannot run gh pr view,
gh pr ready or gh --version, despite explicit user authorization and an ACP
launch configured with --dangerously-skip-permissions. Echo succeeds. Resolve
whether this is configuration or a bug, then restore the required workflow
without weakening unrelated command restrictions.

## Context summary

The parent Lody session is a99663bc-24d2-40b2-99c6-a9ba667cc7af, working in
`/home/leynos/.lody/repos/github---leynos---rstest-bdd/worktrees/a99663bc-24d2-40b2-99c6-a9ba667cc7af`.
The durable VTCode session is
vtcode-zed-session-bbe03cfd-b3c9-4d7a-8792-8d7aecb3b0ca. The session is idle.
Its Lody agent configuration is 46946871-b957-4323-b286-8c0bedf122ce in the
leynos workspace. It launches the Friendli wrapper with the permission-skip
flag; the wrapper loads /home/leynos/.vtcode/vtcode.toml explicitly. That TOML
has no commands override, and the agent configuration has no environment
overrides. The tool-level policy allows exec_command with empty constraints.

The installed binary reports version 0.156.1. Its exact source SHA is not
embedded in that output; do not assert exact binary/source parity. Current
source at root HEAD 4c2d64ea5 has the same exact denial text and documented
command allowlist behavior. No paid provider request or PR mutation is needed
for the experiment.

## Error artefacts

The log is
`/home/leynos/.vtcode/logs/debug-cmd-acp-1788740446687-314863.log`.
Lines 62–68 record ACP permission prompt bypass, safety assessment, then:

```text
command 'gh pr view 662 --repo leynos/rstest-bdd --json isDraft,state,url'
is not permitted by the execution policy
```

The exact source gate is ToolRegistry::prepare_exec_run_request in
crates/codegen/vtcode-core/src/tools/registry/executors.rs. It consults the
command policy before launching a process. CommandPolicyEvaluator::from_config
merges comma-separated VTCODE_COMMANDS_ALLOW_LIST entries into defaults, and
allows_text applies deny rules before allow rules. CommandsConfig::default
does not include gh. Relevant files are commands.rs in vtcode-config and
tools/command_policy.rs plus utils/common.rs in vtcode-core.

## H1: The default command allowlist excludes gh independently of ACP approval

Claim: the configured absence of a gh command allowance explains the denial;
the ACP approval bypass does not override this separate execution policy.
Plausibility: high, given the exact log gate and empty command overrides.

Prediction: the production command evaluator denies the reported gh commands
under defaults, permits ordinary existing commands, then permits only the
specified GitHub workflows when an additive environment override is supplied.
An explicit deny still wins. Any contrary outcome falsifies this hypothesis
or the proposed narrow configuration correction.

## H1 falsification experiment

Alchemist may compile one scratch Rust executable against an already-built
vtcode_core rlib from
`/home/leynos/Projects/VTCode.worktrees/hex-issue-40-read-metadata/target/debug/deps`.
Use that build's Rust toolchain and dependency directory. The program must
call the actual public CommandsConfig::default and CommandPolicyEvaluator,
not reproduce their logic. Store scratch source under /tmp and its compiled
executable under the programme worktree's target/acp-gh-policy-probe directory.
No Cargo registry isolation, repository-wide build, file edits or live agent
turns are allowed. If the existing rlib cannot support this bounded experiment,
report inconclusive with the precise error instead of expanding the build.

Run the executable in separate processes with all six command allow/deny
list/regex/glob environment overrides removed. The baseline should return:

- false for the exact reported gh pr view and gh pr ready commands;
- false for gh --version and gh repo delete leynos/example;
- true for echo ok and cargo check.

Run the same executable with only this additive override:

```text
VTCODE_COMMANDS_ALLOW_LIST=gh pr view,gh pr ready,gh --version
```

The three requested GitHub command forms should become allowed; echo and
cargo remain allowed; gh repo delete remains denied. Finally construct a
local CommandsConfig with an additional deny_list entry gh pr ready and verify
that it defeats the environment grant. Do not execute any command being
classified. Output only command labels and booleans, never environment dumps
or credential contents. Capture the exact command/output and toolchain in
/tmp/vtcode-acp-gh-policy-probe.out.

## Execution order and termination

Run H1 only. Return falsified, not-falsified or inconclusive, identifying which
prediction failed. Root decides whether a new hypothesis or implementation is
needed. The alchemist must not mutate Lody configuration, PRs or sessions.
Root owns any subsequent narrow configuration update and its readback.

## Configuration proposal if H1 survives

Add the tested environment override to the affected Lody agent configuration,
preserving all existing settings. This avoids replacing default TOML lists or
changing command access for every VTCode configuration. Verify stored readback
and explain any requirement for a fresh ACP process. Do not restart/cancel a
session or trigger a paid provider request to test it. No broad gh wildcard or
credential API workaround is required for the reported workflow.

## First experiment outcome

The alchemist returned inconclusive: both existing vtcode_core rlibs failed
linkage with rustc 1.93.0 and E0463, before the evaluator could run. The log is
/tmp/vtcode-acp-gh-policy-probe.out. No classified command executed and no
configuration changed. The user's subsequent report of absent MCP tools and
skills adds a release-provenance check and a discovery-path audit before any
claim that the installed executable contains the pending ACP fixes.
