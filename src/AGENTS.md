# vtcode (binary)

[Root AGENTS.md](../AGENTS.md) | CLI entrypoint, session bootstrap, and agent
runloop wiring. Detailed runloop recovery and allocator notes live in the
[vtcode binary gotchas guide](../docs/development/vtcode-binary-gotchas.md).

## Modules (active bridge: `agent/runloop/unified/webmcp.rs`)

`main.rs` binary entry | `agent/` runloop + subagent dispatch | `cli/` handlers
including opt-in WebMCP serving | `startup/` onboarding | `updater/` downloads
and self-replacement | `codex_app_server/` bridge | `main_helpers/` tracing and
runtime init | `agent/runloop/unified/planning_workflow/tracker_response.rs`
model-facing path boundary |
`agent/runloop/unified/turn/session/interaction_loop_runner/status_refresh.rs`
status/IDE/title cadence |
`agent/runloop/unified/session_setup/hook_approval.rs` workspace lifecycle-hook
approval overlay

## Rules

- Keep the binary thin; runtime logic belongs in `vtcode-core`. Build
  `LifecycleHookEngine` with `new_with_session_gated`, passing
  `workspace_gated = vt_cfg.workspace_lifecycle_hooks` non-empty OR
  `active_primary_agent.contributes_workspace_controlled_hooks()`; the approval
  overlay lives in `session_setup/hook_approval.rs`. See the binary gotchas
  guide for WebMCP bridge and prompt-boundary details.
- Spool preview generation and shell activity classification belong to
  `vtcode-core`; the binary only serializes the typed reference.
- `mimalloc` is the default allocator; `allocator-jemalloc` opts into
  `tikv-jemalloc`. Measure with `vtcode bench-allocator` before changing it;
  see the [allocator guide](../docs/development/ALLOCATOR_MEMORY.md).
- Install `vtcode_ui::tui::panic_hook` before producing output.
- `agent/runloop/` is the single-agent loop;
  `unified/turn/session_loop_runner/mod.rs` is its facade and
  `orchestration.rs` owns the loop body.
  `session_loop_runner/blocked_handoff.rs` owns forced blocked-turn archive
  checkpoints and verified resume handoffs. `session_loop_runner/harness.rs`
  opens and closes canonical session persistence with shared one-shot
  finalization; unexpected exits must emit terminal lifecycle events before
  draining. Keep status-line command input, persistence, preview, and action
  policy behind the private child modules of
  `turn/session/slash_commands/ui/statusline.rs`; workspace-local execution
  summaries use the shared relative-path formatter.
- Keep transcript/modal editor opens on the bounded runtime coordinator, not
  queued `/edit` submissions.
- Keep `/secret` provider/key validation and storage selection, including
  gateway providers, behind `turn/session/slash_commands/secrets/storage.rs`
  and the central resolver.
- Route batched tool metrics through the shared execution helper so every
  executed status is recorded; checkpoint diagnostics use canonical `Usage`
  plus saturating per-turn counters.
- Keep PTY status handoff during stream shutdown separate from output
  rendering; preserve complete current-session tool output for the fullscreen
  `Ctrl+T` viewer, group only contiguous successful command activity in compact
  presentation, suppress transient PTY rows in compact mode while retaining
  expanded live previews, label distinct pipe streams, and keep bounded
  queue-pressure diagnostics visible without duplicate aliases.
- `agent/runloop/unified/turn/compaction/` delegates to
  `vtcode-core::compaction`; reserve segment boundaries with the shared
  transition helper.
- Updates own asset selection, checksum verification, safe extraction, and
  `self_replace`; TUI installs thread `UpdateProgress` callbacks through
  `install_update_reported` for real-time download/extract feedback;
  `main_helpers` owns relaunch context, pre-config legacy migration, and
  runtime initialization.
- Centralize provider-noise sanitization in `turn::provider_noise` and
  `stream_sanitization::StreamSanitizer`.
- Preserve prompt-section ordering and wire-tool shaping invariants; see the
  detailed guide before changing request assembly. Keep clean request and
  continuation history Arc-shared; injected context/few-shot additions and
  provider compaction are intentional copy boundaries.
- Approved-plan turns apply one bounded internal loop allowance at
  initialization and schedule implementation through an explicit internal
  next-turn trigger; auto-permission probe warnings stay queued until the
  assistant tool batch is complete, then flush before recovery directives.
- Preserve planning recovery, approval, interview, and budget-synthesis
  invariants; use the shared `ThreadEvent` contract for telemetry. Ordinary
  loop-limit refusal gets one tool-free synthesis pass, while the absolute hard
  cap remains terminal. Planning gets one deterministic canonical-plan
  synthesis after two empty responses; failures remain resumable and must not
  request more input or advertise implementation without a validated persisted
  plan. Optional event exporters are best effort and must not prevent canonical
  finalization.
- The model picker must derive custom-provider metadata from exact profiles
  while keeping `model`/`models` as the availability allowlist.
- Natural-language persistent-memory saves are handled before the current
  prompt is appended; `remember it`/`this`/`that` may use only the latest
  non-empty assistant answer, never tool output or an older conversation
  window, and still require planner validation plus inline confirmation.
- Ordinary completed turns must publish a non-empty final response through both
  renderer and harness paths; the approved-plan handoff is the explicit
  control-flow exception because its outer loop creates the implementation
  turn. Blocked recovery remains visible. Transient post-tool follow-up failure
  compacts the older prefix, while context-capacity recovery is recognized only
  at `execute_llm_request`, then permits one tool-enabled retry before a
  resumable blocked handoff. Async checkpointing acknowledges consumed steering
  intents only after a `Persisted` history result, never after a throttled
  checkpoint; archive-disabled sessions release in-flight intents without
  marking them durable. Archive-less runner handoffs must omit resume commands.

## Gotchas

The detailed maintainer notes are in
[vtcode-binary-gotchas.md](../docs/development/vtcode-binary-gotchas.md);
startup timing must initialize before tracing and remain opt-in;
`startup::StartupPolicy` keeps metadata read-only/no-auth, ask/`--print`
auth-only, app-server security-only, and theme preference I/O interactive-only;
live status config reloads must invalidate Git/command refresh gates, and
malformed live config must retain the last valid snapshot; blocked-tool fuses
drain the current assistant batch then schedule one tool-free synthesis pass,
blocker live pointers are cleared only by the owning session after the archive
is marked resolved, final session archives retain lightweight last-turn
diagnostics while full progress remains checkpoint-only, direct idle/error
status clears must mark cached status for resynchronization, and DSML parsing
must tolerate whitespace around full-width token separators; anti-blind-editing
counts successful mutations only, carries pending verification across resumed
turns, grants 2 fix-up edits after a failed verifier run (tool-level failures
grant none), admits truncation-only piped verifiers to run without clearing the
gate while chained mutations behind a verifier prefix stay blocked, exempts
planning synthesis from the text block, does not treat `git diff` or piped
checks as verification, and keeps Copilot/batch tracker persistence in sync;
cross-turn no-progress tracking resets on workspace mutations and command
execution; a user exit after a completed non-fallback turn is successful thread
completion, while mid-turn exit remains cancellation; streamed plan markup is
display-suppressed, accepts one final validated `<proposed_plan>` or `<plan>`
marker, and leaves persistence runtime-owned; validation-repair follow-ups use
a bounded pending queue independent of prior text-response streaks;
response-cap stops apply to consecutive text-only responses, use authoritative
compaction-safe turn state, reset only after tool admission (including Copilot
inline execution), remain blocked outcomes, and promote substantive commentary
to the final phase without duplicating renderer or `ThreadEvent` output;
successful tracker rendering must use `tracker_view_lines` so inline
replacement and the TODO panel share one compact tree; model-picker discovery
must preserve legacy-cache recovery and use bounded concurrent provider probes;
active WebMCP pairing displays the exact origin, can issue a non-replacing code
for another configured origin, and reserves `--replace` for revocation; updater
asset URLs must stay on HTTPS GitHub release paths, asset downloads must never
use API credentials, and missing or invalid checksum metadata must abort
installation; interactive palette probes must finish before startup errors
return so OSC replies cannot leak into the shell; settings palette mutations
are field-level writes, and custom-provider or provider endpoint/credential
edits belong to the trusted user layer unless an explicit config file is
selected; failure-like tool outcomes include non-zero commands, which retain
evidence but require bounded diagnosis and a `diagnosis` ReasoningItem;
collapsed output uses one provider-neutral typed turn-scoped notice, cleans
duplicate legacy copies before request assembly, and uses Anthropic-native
lifecycle fields only where supported; keep this file focused and under 30
lines.
