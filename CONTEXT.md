# Diapason Agent Context

This file captures runtime invariants for automation-oriented usage of Diapason.

## Output Contracts

- Use `diapason ask --output-format json` when downstream tooling needs
  structured replies.
- Use `diapason exec --json` for JSONL event streams.
- Use `diapason exec --events <path>` to persist machine-readable transcripts.

## Runtime Introspection

- Discover built-in tool contracts from the binary at runtime:
  - `diapason schema tools`
  - `diapason schema tools --mode minimal`
  - `diapason schema tools --name exec_command --name code_search`

## Safety Defaults

- For autonomous mutation-oriented work, run a read-only rehearsal first:
  - `diapason exec --dry-run "<task>"`
- Treat prompts and MCP CLI values as untrusted input.
- Prefer small, explicit tool arguments over broad/unbounded payloads.

## Context Discipline

- Limit request scope in prompts.
- Prefer focused tool calls and incremental execution.
- Avoid requesting full-file/full-workspace payloads unless required.
