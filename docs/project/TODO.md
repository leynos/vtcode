# Diapason Harness Robustness: Resume After Tool-Call Limit Grant

Grant more tool calls must resume the **same Build turn**, not leave the
harness blocked or leak into Duck. Session logs show a blocked turn with no
final answer, then a new session that burned overlapping README searches and
continued as Duck.

**Steps**

1. Treat overlay Cancel/Esc as deny; keep waiting on leaked
   Tab/CyclePrimaryAgent; re-show on Deferred instead of mapping those to deny
   in `limit_prompts.rs`.
2. After a grant, retry the pending tool call with the increased budget and
   emit a harness grant event — do not mark `SessionLimitNotIncreased` /
   Blocked.
3. After a loop-limit grant, keep tools enabled and continue the same agent; do
   not arm tool-free recovery.
4. Snapshot the write-capable agent at prompt time; restore Build if Duck/Plan
   leaked in during the grant.
5. Lock Tab/mode switches for overlays, Building, Recovery, and Blocked — not
   only transient handoff states.
6. Persist `primary_agent` on every switch and restore it on resume; do not
   drop to Duck via `/new` after a live blocked handoff.
7. Clear Blocked UI after grant; do not convert a grant-in-flight turn into
   `COMPLETED_TURN_NO_RESPONSE_REASON`.
8. Persist the granted session fuse across `set_limits` / `start_turn`.
9. Cap same-file `code_search` reuse (README churn in session 2) and tell the
   model to use existing outputs after a grant.
10. Show explicit user copy: current agent, grant vs deny, and what to do next.

**Relevant files**

- `limit_prompts.rs` — grant vs deny
- `overlay_prompt.rs` — overlay wait
- `turn_loop_helpers.rs` — loop-limit resume
- `orchestration.rs` — blocked UI, agent persist, new session
- `mode_switch_guard.rs` — Tab lock
- `read_guard.rs` — same-path search cap

**Verification**

1. Overlay noise is not a deny; genuine Deny still synthesizes.
2. Grant retries the pending call as Build.
3. Tab is locked while Building/Blocked/overlay-active.
4. Archive restore keeps Build; session fuse survives the next turn.
5. Repeat `code_search` on one file reuses or stops after a small cap.
6. `cargo nextest run` on limit-prompt, blocked-handoff, primary-agent,
   read-guard, and UI Tab-lock tests.

**Decisions**

- Restore the pre-grant write-capable agent **and** lock Tab during
  grant/blocked/active turn.
- Idle Tab after a finished turn stays user-controlled.
- Out of scope: raising default loop/session caps; rewriting Duck’s spec.
