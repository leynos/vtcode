# Friendli issue 36 captures

These are sanitized copies of six synthetic-only probe captures recorded on
2026-09-05. They contain no authorization header, API key, workspace content,
or user data. Request JSON is retained because every prompt and tool schema is
synthetic.

The six capture groups are `function`, `custom`, `custom-format-required`,
`custom-named`, `custom-text-format`, and `planner`. The first five exercise
Responses tool compatibility; `planner` preserves the adjacent baseline used
by the same investigation. Response bodies are exact captures. Header files
retain only the HTTP status and safe response metadata present in the source
capture.

Observed results must not be upgraded beyond the wire evidence:

- `function` completed with one `fixture_echo` function call. Its streamed and
  terminal call IDs differ, which is the issue 35 remapping fixture.
- `custom` and `custom-format-required` returned HTTP 500. They are failed
  compatibility probes, not successful custom calls.
- `custom-named` and `custom-text-format` returned HTTP 200 but completed with
  prose and no custom call. They are also failed compatibility probes.
- `planner` is a valid synthetic Chat Completions response and says nothing
  about Responses tool support.

Do not replay these files against the paid endpoint. Offline tests may serve
them from localhost or VidaiMock, but replay success only proves VT Code's
handling of the captured shape—not current Friendli compatibility.
