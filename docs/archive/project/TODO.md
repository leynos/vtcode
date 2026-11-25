# Diapason TODO scratchpad

Out of curiosity, since I don't understand this too much. Since it is terminal
coding, could this be ported to work on a vt520 terminal and just use the
terminal as a chat bot?

Right now I am running a python script and API key to run an AI bot on my
vt520. It was a neat little project - I had help from Gemeni

===

Areas that are complex The most intricate parts are likely:

     1. core_tui/session.rs
        Main state transitions, layout, rendering, and interaction coordination.
     2. Transcript rendering and caching
        Reflow, scroll behaviour, tool blocks, PTY output, overlays, and cache
        invalidation interact heavily.
     3. Input ownership
        Normal input, popups, approval prompts, search, and fullscreen review each have
        different routing rules.
     4. Async integration
        Terminal events, agent events, PTY events, and redraw requests must be
        coordinated without blocking the runtime.
     5. Theme and contrast behaviour
        Theme changes affect normal text, accents, syntax highlighting, status colours,
        overlays, and accessibility requirements.

==> improve

===

## Historical Checkpoint Evidence and Personal Notes

diagnose and improve diapason harness based on the session run log.

/Users/vinhnguyenxuan/Developer/learn-by-doing/diapason/.diapason/checkpoints/turn_1032.json
/Users/vinhnguyenxuan/Developer/learn-by-doing/diapason/.diapason/checkpoints/turn_1031.json

Diagnosis (from checkpoint evidence) Turns analysed: turn_1030.json (108,346
in-tok, 12 tools, 56s), turn_1031.json (32,280 in-tok, 4 tools, 33.7s),
turn_1032.json (16,149 in-tok, 0 tools, 14.3s).

## : 1

Finding: Prompt cache never warm Evidence: cached_input_tokens: 0,
cache_creation_tokens: 0 on all turns — turn 1030 paid full price for 108K
input tokens
─────────────────────────────────────────────────────────────────────────────────

## : 2

Finding: Preview budget exhaustion returns zero visibility Evidence: Even
trivial/empty commands returned preview_budget_exhausted with empty output;
spool_path: null, byte_count: 7751 = result dropped entirely ( neither inline
nor spooled)
─────────────────────────────────────────────────────────────────────────────────

## : 3

Finding: completion_state: "unknown" on successful (exit-0) exec results
Evidence: Diagnostics can't distinguish clean completion from timeout
─────────────────────────────────────────────────────────────────────────────────

## : 4

Finding: model_visible_output_bytes ≪ raw_spooled_bytes Evidence: Turn 1031:
19,325 visible vs 44,129 spooled (~44% of evidence reached the model)
─────────────────────────────────────────────────────────────────────────────────

## : 5

Finding: Low-signal detector misses duplicate listings Evidence:
low_signal_tool_calls: 0 despite 3 overlapping find invocations in one turn
─────────────────────────────────────────────────────────────────────────────────

## : 6

Finding: Diagnostics schema instability Evidence: Turn 1030 has elapsed_ms:
null, requested_tool_calls: null — no trend analysis possible across turns
─────────────────────────────────────────────────────────────────────────────────

## : 7

Finding: files array always empty (file_count: 0) even for file-reading turns
Evidence: Session replay can't show touched files

===

improve colouring of grouped tool call commands and wording highlight

'/Users/vinhnguyenxuan/Documents/diapason-resources/bugs/Screenshot 2026-09-02
at 21.09.57.png'
'/Users/vinhnguyenxuan/Documents/diapason-resources/bugs/Screenshot 2026-09-02
at 21.09.55.png'

______________________________________________________________________

improve and fix UI
'/Users/vinhnguyenxuan/Documents/diapason-resources/bugs/Screenshot 2026-09-02
at 21.00.54.png'

______________________________________________________________________

check and fix

• The turn is blocked before success could be confirmed. The available history
and outputs are retained; resume the request to continue.
------------------------ Info ------------------------- Recovery tool-call
limit reached after 3 blocked calls. Last blocked call: 'Run command'. Tools
remain disabled while the recovery response is finalized. Blocked handoff:
/Users/vinhnguyenxuan/Developer/learn-by-doing/diapason/.diapason/tasks/current_blocked.md
Blocked handoff:

/Users/vinhnguyenxuan/Developer/learn-by-doing/diapason/.diapason/tasks/blockers/session-diapason-20260902t14281-3z_732254-54385-20260902T144229Z.md

note: i want you to fix and improve diapason harness based on the session run
log. The harness should be able to handle blocked calls more gracefully,
provide better feedback to the user, and ensure that the session can resume
smoothly after a blockage. Additionally, improve the UI to clearly indicate
when a turn is blocked and what actions the user can take to resolve it. not do
it yourself

______________________________________________________________________

fix broken tool call rendering

'/Users/vinhnguyenxuan/Documents/diapason-resources/bugs/Screenshot 2026-09-02
at 22.40.46.png'

===

===

check /config listing some time can not scroll to last items, it get stuck even
though the list is longer than the screen. The harness should ensure that the
scrolling behaviour in the /config listing is smooth and allows users to reach
the last items without getting stuck. Additionally, provide visual indicators
or feedback to inform users when they have reached the end of the list or if
there are more items to scroll through.

'/Users/vinhnguyenxuan/Documents/diapason-resources/bugs/Screenshot 2026-09-03
at 16.58.38.png'

===

fix propose plan mode markdown is not rendering correctly, it should be able to
render the markdown content properly and display it in a readable format. The
harness should ensure that any markdown content is parsed and rendered
accurately, preserving formatting, headings, lists, and other elements.
Additionally, provide clear feedback to the user if there are any issues with
rendering the markdown content.

'/Users/vinhnguyenxuan/Documents/diapason-resources/bugs/Screenshot 2026-09-04
at 10.54.05.png'

______________________________________________________________________

check plan mode: diapason on user approval confirmation dialog -> accept and
implement -> the agent use auto mode -> then later the agent switch to propose
plan mode, but not implement it? User has to nudge `start implement` manually.
The harness should ensure that when the agent switches to propose plan mode, it
automatically implements the proposed plan without requiring additional user
intervention. Additionally, provide clear feedback to the user about the
current mode and any actions they need to take to ensure the plan is
implemented correctly.

log:
/Users/vinhnguyenxuan/Developer/learn-by-doing/diapason/.diapason/sessions/session-diapason-20260904T034530Z_179873-58689

===

check and fix diapason agent keep getting turn blocked. repeatedly. deep dive
and fix.

CRITICAL.

    ====================================================== Warning
    ====================================================== Mutation blocked until
    verification: a mutation batch from an earlier turn is still awaiting a
    successful build, test , lint, or compile command
    =====================================================================================================================

    • The turn is blocked because verification is still pending. Inspection-only
    checks do not clear the verification gate; run cargo check --locked or the
    relevant cargo nextest run command, then resume the request.
    ------------------------------------------------------- Info
    -------------------------------------------------------- Turn blocked after
    repeated unverified assistant responses; verification is still pending.

a non-zero exit status; the bounded output does not establish a more specific
cause. Next action: Inspect the reported error and retry with corrected
arguments or a narrower scope.

• Ran cargo check --locked

log:
/Users/vinhnguyenxuan/Developer/learn-by-doing/diapason/.diapason/sessions/session-diapason-20260904T034530Z_179873-58689

______________________________________________________________________

small details: on "/" splash command pallate suggestion, pressing "enter"
should just accept the suggestion and populate into the chat input box, instead
of executing the command immediately. This allows users to review and edit the
suggestion before sending it, improving usability and reducing accidental
command execution. Check how "tab" key is used for suggestion selection and
ensure that the behaviour is consistent with user expectations. Additionally,
provide visual feedback to indicate that the suggestion has been accepted into
the input box, and allow users to easily modify or cancel the input before
sending it.

______________________________________________________________________
