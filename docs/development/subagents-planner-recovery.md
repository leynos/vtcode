# Subagent planner recovery

Delegated child agents execute the bounded task supplied by their parent. The
child configuration therefore sets `agent.harness.orchestration_mode` to
`Single`, even when the parent uses `PlanBuildEvaluate`. The parent setting is
unchanged, and the child continues to inherit its existing permission, model,
reasoning, tool, and turn-limit policies.

This boundary prevents a child from entering a second planner phase after
full-auto starts. It is child-specific: ordinary top-level runs still use the
configured harness orchestration mode.

Delegated children also skip the automatically generated `analyze`/`change`/
`verify` tracker. This prevents a completed read-only task from being held open
by a synthetic verification step. An explicitly created child tracker still
enforces its outstanding steps, and primary-agent tracking is unchanged.

## Diagnose structured-response failures

Planner, evaluator, and replanner parse failures report metadata instead of
model-generated content. A diagnostic includes:

- the structured-response phase;
- a normalized finish reason;
- the response content size in bytes;
- a parse class: `empty`, `syntax`, `schema`, `truncated`, or `io`; and
- the parser line and column when available.

The diagnostic never includes the response body or the provider text carried
by `FinishReason::Error`. This keeps terminal child status useful without
echoing model-generated values.

## Failed child history

A failed `execute_task` remains a failed child run. Before returning that
error, the child loop finalizes the session archive from the runner's current
messages. If archive finalization also fails, the execution error remains the
reported failure and the archive error is logged separately.

The runner publishes the accepted task to its thread snapshot before fallible
setup begins. A failed planner or provider request therefore cannot leave the
archive containing only the earlier bootstrap messages.

`ChildRecord` records the terminal `Failed` status, completion timestamp, and
a control-character-normalized error chain capped at 2,048 characters. The
record retains its archive path and effective child configuration for status
and ACP projections.

## Verify the contracts

Run the focused configuration, parser, property, and terminal-error tests with
`cargo nextest run -p vtcode-core` before the wider changed-crate gate. The
tests pin every built-in child to `Single`, preserve the parent's mode, cover
each parse class, reject response-content echoing, and bound arbitrary Unicode
diagnostics.
