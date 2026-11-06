# Instruction discovery boundary

Diapason discovers authored guidance from three scopes: the user config
directory, the project root, and explicit `agent.instruction_files` patterns.
The discovery functions in `diapason-core` accept an explicit home directory so
callers can control whether user-scope guidance participates.

## Home-directory injection

| Function                                                                                      | Purpose                                                            |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| `read_project_doc_with_home(cwd, max_bytes, home_dir)`                                        | Read the project documentation bundle for `cwd`.                   |
| `build_instruction_appendix_with_context_and_home(config, active_dir, match_paths, home_dir)` | Build the instruction appendix string.                             |
| `load_instruction_appendix_with_home(config, active_dir, match_paths, home_dir)`              | Load the appendix bundle, including the persistent-memory excerpt. |
| `IncrementalSystemPrompt::with_instruction_home(instruction_home)`                            | Pin the home directory used by incremental prompt refreshes.       |

The `_with_home` variants are the testable boundary. The convenience wrappers
(`read_project_doc`, `build_instruction_appendix_with_context`,
`load_instruction_appendix`) pass `dirs::home_dir()`.

## `None` does not isolate a caller

Passing `None` disables user-scope instruction discovery, but it does not
isolate a caller from its environment. Workspace-scope discovery still walks
from the working directory up to the resolved project root, and the project
root falls back to the filesystem root when no `.git` marker is found.

A test fixture that must not observe the developer's home directory should
therefore also be its own project root, for example by containing a `.git`
marker. Production callers pass the real home directory; tests pass an isolated
temporary home and a fixture root.

## Include and exclude error contract

- An `agent.instruction_files` pattern that cannot be resolved or compiled is
  reported as a warning and skipped. One bad include does not abort discovery
  or hide the rest of the workspace guidance.
- An invalid `agent.instruction_excludes` pattern is fail-closed: discovery
  stops with an error rather than loading files that should have been excluded.

## Renamed APIs

The 0.157.0 release renames colour-related identifiers to British spelling. See
the [0.157.0 release notes](../releases/0.157.0.md) for the full list. The two
public renames most likely to affect callers are `RiskLevel::color_code` →
`RiskLevel::colour_code` and `Alignment::Center` → `Alignment::Centre`.
