# Session Event Persistence

Diapason stores the authoritative `diapason_exec_events::ThreadEvent` stream
for interactive and exec sessions at:

```text
<state>/workspaces/<workspace-hash>/sessions/<session_id>/events.jsonl
```

Workspace sessions are namespaced below `<state>/workspaces/<workspace-hash>/`;
the state directory is `$XDG_STATE_HOME/diapason/` by default. The session
directory also contains the manifest, turn index, and derived artifacts. The
canonical event sink uses bounded, non-blocking handoffs to a dedicated
blocking drain. Filesystem I/O does not run on the async executor. If a bounded
queue fills, persistence fails closed and the run cannot be reported as
successful; accepted events are drained before the failure is returned. A run
may be reported as successful only after its terminal `thread.completed` event
and the canonical drain have completed successfully.

`agent.harness.event_log_path` and exec `--events` are explicit compatibility
exports. They are optional, independently bounded, and may report drops or
write failures without changing the canonical event contract. The
workspace-scoped state root prevents sessions from different repositories from
sharing a retention boundary. ATIF and Open Responses exports configured by the
interactive harness are derived under `<session>/derived/`.

Retention defaults to 50 sessions or 30 days. It operates after session
closure/startup on a blocking task, preserves active/current sessions, skips
symlinks, and removes only validated direct children of the sessions root.

Verification:

```text
cargo nextest run -p diapason-memory
cargo nextest run -p diapason-core -E 'test(/session_store_sink|event/)'
cargo nextest run -p diapason -E 'test(/harness/)'
```
