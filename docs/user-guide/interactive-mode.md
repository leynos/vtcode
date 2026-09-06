# Interactive Mode Reference

The VT Code terminal UI includes an interactive mode that combines keyboard-first navigation with quick commands for agent control. This page consolidates the shortcuts, input modes, and background execution behaviours available while you are connected to a session.

## Keyboard Shortcuts

> Keyboard shortcuts may vary slightly by platform and terminal emulator. Press `?` on an empty input line while VT Code is running to open a quick shortcut overlay.

### General Controls

| Shortcut                                    | Description                                                                     | Context                                                                                                                                   |
| :------------------------------------------ | :------------------------------------------------------------------------------ | :---------------------------------------------------------------------------------------------------------------------------------------- |
| `Ctrl+C`                                    | Cancel the current generation or command. Press twice to terminate the session. | Works during prompts, tool execution, and streaming replies.                                                                              |
| `Ctrl+D`                                    | Exit VT Code interactive mode.                                                  | Sends EOF to the shell integration.                                                                                                       |
| `Ctrl+L`                                    | Clear the terminal screen while keeping the conversation history.               | Useful for refreshing when output is cluttered.                                                                                           |
| `Ctrl+T`                                    | Open or close Transcript Review (default binding).                               | Works in inline and fullscreen modes; if unbound, it remains the readline transpose shortcut.                                             |
| `Alt+O`                                     | Open Transcript Review (compatibility alias).                                    | Useful when the terminal does not deliver `Ctrl+T`; mouse capture is required for clickable hints.                                        |
| `Alt+T`                                     | Toggle compact or expanded tool summaries.                                      | Rebindable; the default compact presentation keeps live command output bounded.                                                         |
| `Alt+G`                                     | Toggle the TODO task panel.                                                     | Rebindable via `toggle_task_panel`; works even when `ui.show_task_panel` is off.                                                          |
| `Ctrl+O`                                    | Copy the latest assistant response, or the complete Transcript Review.          | In review, always copies the complete ANSI-free conversation; otherwise it uses the latest-response behaviour.                            |
| `Ctrl+A`                                    | Move cursor to start of input line.                                             | UNIX/readline-style editing.                                                                                                              |
| `Ctrl+E`                                    | Move cursor to end of input line (or open external editor when input is empty). | Uses `tools.editor` config, then `VISUAL`/`EDITOR`. Configure it with `/config tools.editor`.                                             |
| `Ctrl+Home`                                 | Jump to the oldest visible transcript content.                                  | Fullscreen rendering only.                                                                                                                |
| `Ctrl+End`                                  | Jump back to the latest transcript content and resume follow mode.              | Fullscreen rendering only.                                                                                                                |
| `Ctrl+W`                                    | Delete the previous word.                                                       | UNIX/readline-style editing.                                                                                                              |
| `Ctrl+U`                                    | Delete from cursor to line start.                                               | UNIX/readline-style editing.                                                                                                              |
| `Ctrl+K`                                    | Delete from cursor to line end.                                                 | UNIX/readline-style editing.                                                                                                              |
| `Alt+Left/Right`                            | Move cursor by word.                                                            | UNIX/readline-style navigation.                                                                                                           |
| `Ctrl+R`                                    | Reverse search the command history.                                             | Matches previous prompts and bash commands.                                                                                               |
| `Ctrl+V` or `Alt+V`                         | Paste a clipboard image through VT Code.                                        | App-level shortcut; works only in image-enabled sessions. Use your terminal's paste shortcut for text.                                     |
| `Ctrl+Z` (Unix)                             | Suspend VT Code to the shell; run `fg` to resume.                               | Job-control support for terminal workflows.                                                                                               |
| `Up/Down arrows`                            | Navigate through command history.                                               | Recall previous prompts or commands.                                                                                                      |
| `Esc` + `Esc`                               | Open the rewind picker for checkpoint restore or summarize actions.             | Idle context only (while no task/PTY is running).                                                                                         |
| `Enter`                                     | Queue the current input.                                                        | Plain input box only.                                                                                                                     |
| `Tab`                                       | Accept the visible inline suggestion; on an empty idle composer, cycle primary agents; otherwise queue the current input. | Plain input box only. Disabled while a turn is processing (shows a notice). |
| `Ctrl+Enter`                                | Queue the current draft (batchable).                                              | Idle: submits the current draft immediately (or the newest queued message if the draft is empty). Active: joins the visible queue; consecutive text-only messages are batched into one turn. `/stop`, `/pause`, and `/resume` are handled immediately instead of being queued. |
| `Shift+Tab` or `Alt+M`                      | Cycle primary agents.                                                           | Switches between available main-session agents. Disabled while a turn is processing (shows a notice). |

### Multiline Input

| Method               | Shortcut       | Context                                                                                           |
| :------------------- | :------------- | :------------------------------------------------------------------------------------------------ |
| Quick escape         | `\` + `Enter`  | Works across supported terminals.                                                                 |
| macOS default        | `Option+Enter` | Default multiline binding on macOS terminals.                                                     |
| Native or configured | `Shift+Enter`  | Works natively in some terminals and is available after `/terminal-setup` in supported terminals. |
| Control sequence     | `Ctrl+J`       | Inserts a line feed for multiline editing.                                                        |
| Paste mode           | Paste directly | Ideal for code blocks or long transcripts.                                                        |

> Tip: `Shift+Enter` works natively in `Ghostty`, `Kitty`, `WezTerm`, `iTerm2`, and `Warp`. Run `/terminal-setup` in supported terminals such as `VS Code`, `Alacritty`, or `Zed` when you want VT Code's guided setup flow.

### Clipboard Paste

- `Ctrl+V` and `Alt+V` are VT Code shortcuts for image paste. They read a clipboard image and attach it to the current draft when the selected session supports image input.
- Text paste still uses the terminal bracketed paste path. In many terminals this is `Ctrl+Shift+V`; use the paste shortcut configured by your terminal.
- If the selected model does not support image input, VT Code rejects both image paste and image submission, then shows a warning.
- On WSL, VT Code first tries direct Linux clipboard access. If that cannot read an image, it tries a Windows clipboard image fallback through PowerShell.

### Quick Commands

| Shortcut                     | Description                                                | Notes                                                                                  |
| :--------------------------- | :--------------------------------------------------------- | :------------------------------------------------------------------------------------- |
| `#` at start of input        | Access custom prompts.                                     | Opens quick picker to select and run custom prompts directly from input bar.           |
| `/` at start of input        | Issue a slash command.                                     | Run `/help` or `/slash-commands` in a session to list everything available.            |
| `!` at start of input        | Enter Bash mode.                                           | Runs shell commands directly and streams their output.                                 |
| `@` within input             | Open file picker.                                          | Triggers file path autocomplete and picker to quickly reference files in your message. |
| `@agent-<name>` within input | Open subagent picker and insert an explicit agent mention. | Use `@agent-<plugin>:<name>` for plugin-provided agents.                               |
| `Alt+P` / `Option+P`         | Generate an inline prompt suggestion.                      | Shows a ghost-text completion in the composer; `Tab` accepts it.                       |

## Fullscreen Rendering

When VT Code is running in alternate-screen mode, the transcript and composer use a fixed fullscreen layout similar to terminal applications such as `vim` or `less`. The input stays pinned at the bottom, mouse handling is internal to VT Code, and the tool-output viewer/search happens inside the app instead of your terminal scrollback.

### Fullscreen Navigation

| Shortcut        | Description                                                   |
| :-------------- | :------------------------------------------------------------ |
| `PgUp` / `PgDn` | Scroll the live transcript by half a page.                    |
| `Ctrl+Home`     | Jump to the oldest transcript content.                        |
| `Ctrl+End`      | Jump to the latest transcript content and resume auto-follow. |
| Mouse wheel     | Scroll the live transcript when mouse capture is enabled.     |

### Transcript Review

Press the configured review binding (default `Ctrl+T`) to open or close
Transcript Review. The review composes the ordered user messages, assistant responses,
reasoning, summaries, warnings, errors, and complete command captures. The
normal transcript remains compact and bounded while the review retains full
PTY captures and distinct pipe stdout/stderr streams.

Successful command rows are grouped only when they are contiguous in rendered
order. The visible shortcut and `click to expand` suffix is styled
and clickable when mouse capture is enabled; clicking it focuses the first
command in that group. Other transcript clicks continue to support selection
and links. Successful file writes and edits end the command group and show a
separate glance row with the affected path, `(+N -M)` counts, and numbered diff
lines; the complete result remains available in the review and agent history.

Rich rendering is the default and reuses the transcript's colours, links, and
width-aware wrapping. Press the configured render-mode binding (default `R`)
to switch to ANSI-free raw rendering for
copying or export. Copying with `Ctrl+O`, opening the editor with `v`, or
handing the review to native scrollback with `[` always uses the complete
ANSI-free conversation text.

| Shortcut                   | Description                                                                              |
| :------------------------- | :--------------------------------------------------------------------------------------- |
| `r`                        | Toggle rich and raw rendering.                                                           |
| `/`                        | Start a case-insensitive conversation search.                                             |
| `Enter`                    | Commit the current search and jump to the first match.                                   |
| `Esc`                      | Cancel the active search, or close the viewer when search is idle.                        |
| `n` / `N`                  | Jump to the next or previous search match.                                               |
| `j` / `k` or `Up` / `Down` | Scroll one line.                                                                         |
| `Ctrl+U` / `Ctrl+D`        | Scroll half a page.                                                                      |
| `Ctrl+B` / `b`             | Scroll a full page up.                                                                   |
| `Ctrl+F` / `Space`         | Scroll a full page down.                                                                 |
| `g` / `Home`               | Jump to the top.                                                                         |
| `G` / `End`                | Jump to the bottom.                                                                      |
| `[`                        | Hand the complete conversation to the terminal's native scrollback until you return.    |
| `v`                        | Write the complete conversation to a temporary file and open it in your configured editor.|
| `q`, `Esc`, or the configured review binding | Close Transcript Review.                                                   |

### Mouse Capture, Copy, and tmux

- `ui.fullscreen.mouse_capture = false` keeps fullscreen rendering but returns click-and-drag selection to the terminal. This also disables in-app wheel scrolling, click-to-expand, click-to-position, and link activation.
- `ui.fullscreen.copy_on_select = false` disables automatic clipboard copy after an in-app text selection. Manual copy shortcuts still work.
- Copying prefers native clipboard helpers (`pbcopy` on macOS, `xclip`/`xsel`/`wl-copy` on Linux, `clip.exe` on Windows) and falls back to the OSC 52 escape sequence. When no strategy succeeds, the input status row shows a `Copy failed` notice instead of a false success.
- Dragging a selection to the top or bottom edge of the transcript auto-scrolls it, extending the selection onto newly revealed lines.
- `ui.fullscreen.scroll_speed` multiplies mouse-wheel scrolling without affecting `PgUp`/`PgDn`.
- Inside tmux, enable mouse support with `set -g mouse on` if you want wheel scrolling and other mouse actions to reach VT Code.

The compact presentation and review affordances are configurable with
`ui.tool_display_mode`, `ui.transcript_review.show_hints`,
`ui.transcript_review.show_shortcut_guide`, `ui.transcript_review.show_close_button`,
and the existing `ui.keybindings.open_transcript_review` and
`ui.keybindings.toggle_transcript_render_mode` entries. All review controls
are enabled and compact display is the default.
- Avoid fullscreen rendering in `tmux -CC` sessions. iTerm2's control-mode integration does not handle alternate-screen mouse capture reliably.

### Session Context Commands

- `/continue` resumes the most recently archived session in a new conversation. Equivalent to `vtcode --continue` from the CLI.
- When a turn is blocked (repeated tool denials hit the fuse), the TUI shows a `Blocked` header badge, `Blocked • continue to retry • /resume • details: .vtcode/tasks/current_blocked.md` footer hint, and a transcript banner. Type `continue` with new guidance, describe alternative instructions, or run `vtcode --resume <session>` from a terminal. One attempt before the fuse trips, the runtime warns with the remaining attempts and a per-tool remedy hint.
- `/compact` compacts the current session history immediately when you want to shed context manually.
- `/compact edit-prompt` and `/compact reset-prompt` manage the saved default prompt for manual compaction requests.
- For providers with native Responses compaction, VT Code uses the provider-owned compacted state.
 - For local fallback compaction, VT Code rebuilds history around one structured summary plus retained recent user messages, then injects the session memory envelope.
 - Automatic compaction also keeps the newest complete protocol groups verbatim in a continuity tail of approximately 20,000 estimated tokens. An interrupted trailing tool call is omitted rather than replayed.
 - Switching the main session model or provider mid-conversation (via `/model`) automatically compacts the existing history before the next turn, so the newly selected model starts from a summary instead of the outgoing model's raw trace. The previous-response chain is cleared so the new model is not chained to the old Responses/cache identity. Reselecting the same model leaves history untouched.
 - `context.dynamic.retained_user_messages` controls that retained-user-message budget; the default is `4`.
- On the local fallback path, VT Code also deduplicates older repeated single-file reads before summarization so the newest read stays available without repeatedly bloating the prompt.
- `/fork` opens the archived-session picker. After selecting a session, VT Code now asks whether the new session should start from the full copied transcript or from a summarized fork.
- A summarized fork starts from the same compacted handoff shape VT Code uses for local compaction: structured summary, retained user prompts, and the memory envelope.
- If a session stopped because it hit the local `max_budget_usd` limit, resuming it offers three choices: continue from the saved summary, continue with the full transcript after an explicit higher-cost warning, or start fresh.
- `/agents` opens the subagent manager for creating, inspecting, editing, deleting, and browsing active delegated agents. New scaffolds use VT Code tool ids in frontmatter.
- `/agent` opens the active-agent inspector. Selecting a child agent opens a modal over the current session instead of switching threads.
- On an empty idle composer, `Tab` cycles primary agents and wraps back to the first agent.
- The active primary agent is displayed in the session header badge and influences the session's instructions, model, granular permission policy, and tool access.
- Mode switches are locked while a turn is actively processing. Pressing `Tab`/`Shift+Tab`/`Alt+M` (or running `/mode`/`/plan`) during a turn is dropped with a notice and applies only once the turn finishes. This keeps the agent's mode and tool-access state consistent for the duration of a turn; the in-turn automatic planning intent detection is unaffected.
- `/subprocesses` opens the Local Agents drawer for delegated agents and managed background subprocesses.

## Scheduled Prompts And Reminders

- VT Code also recognizes narrow reminder phrases in chat such as `remind me at 3pm to ...`, `in 45 minutes, ...`, `what scheduled tasks do I have?`, and `cancel <job id|name>`.
- Session-scoped scheduled prompts fire only at idle boundaries so they do not interrupt an in-flight turn.
- For full behaviour, limits, and CLI examples, see [Scheduled Tasks](./scheduled-tasks.md).

## Vim Mode

VT Code supports an optional Vim-style prompt editor.

- Set `ui.vim_mode = true` to enable it by default for new sessions.
- Use `/vim`, `/vim on`, and `/vim off` to change the current session only.
- Supported modes are `INSERT` and `NORMAL`.
- Supported subset includes motions, change/delete/yank operators, `f/F/t/T`, text objects, `p/P`, `J`, and repeat with `.`.
- VT Code does not implement visual mode, macros, or multiple registers; yanks reuse the single session clipboard.
- VT Code-specific prompt controls still win when relevant, including `Enter`, `Tab`, `Ctrl+Enter`, `/`, `@`, and `!`.

## Prompt Suggestions, Tasks, and Jobs

- `/suggest` opens a prompt-suggestion picker built from recent session context such as task state, active jobs, recent errors, and recent file activity.
- `Alt+P` requests one inline ghost-text suggestion for the current draft. If a ghost suggestion is visible, `Tab` accepts it; otherwise `Tab` keeps its normal queue behaviour.
- VT Code routes prompt suggestion generation through `agent.prompt_suggestions` and falls back to deterministic local suggestions when the provider, model, or endpoint cannot service the request.
- LLM-backed prompt suggestions can consume tokens. When `agent.prompt_suggestions.show_cost_notice = true`, VT Code shows a one-time reminder in the session before the first LLM-backed inline suggestion.
- Picking a suggestion inserts it into the composer. Empty drafts are replaced; non-empty drafts keep their content and append the suggestion after a blank line.
- `/tasks` toggles the dedicated TODO panel. It is fed directly from `task_tracker` output and remains independent from the tool-summary display mode. Successful updates use the same compact tree in the transcript and panel; the panel header carries the checklist title and `completed/total` progress while step metadata remains available in structured tracker data. `Alt+G` toggles the panel from anywhere, and `ui.show_task_panel` controls whether it auto-shows when a plan is approved.
- `/jobs` opens the active/background jobs picker for PTY-backed command sessions.
- In `/jobs`, `Enter` or `Ctrl+R` focuses the selected job output, `Ctrl+P` previews a snapshot modal, and `Ctrl+X` sends an interrupt to the selected job.
- Pressing `Enter` on an empty draft opens `/jobs` when active jobs exist; otherwise VT Code keeps the normal empty-enter behaviour.

## Active Run Steering

When a task is already running, VT Code keeps the active turn alive and lets you queue or steer input:

- `Enter` and `Tab` queue the current input for later processing; queued messages dispatch one per turn in FIFO order.
- `Ctrl+Enter` queues the current draft as a *batchable* message. Consecutive text-only Ctrl+Enter messages queued while a turn runs are joined into a single combined prompt for the next turn. Slash commands other than `/stop`, `/pause`, and `/resume` are queued non-batchable so their intent is preserved; `/stop`, `/pause`, and `/resume` take effect immediately instead of being queued.
- Queued inputs appear in an overlay above the composer in FIFO order (oldest on top, newest directly above the input). `Shift+Left` (tmux) or `Alt+Up` pops the newest queued message back into the composer for editing. Up to five messages are shown, plus a `+N more queued` line when more are pending.
- `/pause` pauses the active run at the next model/tool/approval boundary.
- `/resume` resumes a paused run while it is active. When idle, `/resume` still opens archived sessions.
- `/stop` still cancels the active run immediately.
- `/compact` still works only while the session is idle; it rewrites the stored conversation context for the next turn instead of interrupting the active run.
- Follow-up steering is assigned an internal intent ID and is checkpointed with the session. Consumed instructions are marked applied only after their tagged user message is written, so a restart does not replay an already durable instruction; identical text with a different intent remains distinct.
- `/fork` is available while idle and creates a new archived session, leaving the current session unchanged.
- `Ctrl+B` starts or stops the configured default background subagent when background mode is enabled and `default_agent` is set. Otherwise it opens the Local Agents drawer and shows setup guidance.
- `Alt+S` opens or focuses the Local Agents drawer.
- When the composer is empty and local agents exist, `Down` opens the Local Agents drawer. `Up` and `Down` keep normal history navigation once history traversal is active.
- In the active-agent and subprocess inspectors, `Esc` closes the overlay, `Ctrl+R` reloads it, `Ctrl+K` requests a graceful stop, and `Ctrl+X` force-cancels the selected subprocess.
- Foreground `!` commands keep their status in the input/status area, and `Esc` collapses verbose output without killing the job.

## PR Review Status

- On GitHub-backed repositories, the header can show a PR review badge such as `PR: ready`, `PR: reviewed`, or `PR: outdated`.
- VT Code uses read-only `gh` inspection for this status. If `gh` is missing or unauthenticated, the header shows the appropriate CTA instead of failing the session.
- The badge refreshes as branch and HEAD state change, and warnings appear when your review is outdated or you do not have write access.

## Planning Workflow Notes

- The built-in `plan` primary agent is read-oriented and intended for repository exploration, trade-off discussion, and proposal drafting.
- `/plan` starts or continues the planning workflow command; it is not a session state selector.
- The agent emits planning output in `<proposed_plan>...</proposed_plan>` blocks.
- `task_tracker` mirrors checklist state with plan sidecars where planning artefacts are enabled.
- When you are ready to implement, switch to a build-oriented primary agent such as `build` or `auto`.
- During the planning workflow the footer shows a `Planning...` stage status, and while an approved plan executes it shows `Building...`. These stage states keep the composer usable so you can queue or steer input between turns; the animated spinner continues while tools execute underneath.
- Plan approval offers three choices: implement in the current context, clear transient context
  and implement with a fresh thread, or stay in Plan mode. Both implementation choices preserve
  the session's existing confirmation policy. The fresh path preserves the plan and task tracker
  while resetting the transcript and tool budget.

## Command History

VT Code keeps a command history scoped to the working directory. The history resets when you clear it manually or start a new directory session.

- Cleared with the `/clear` command.
- Use the arrow keys to navigate between entries.
- History expansion via `!` is disabled by default to prevent accidental execution.

### Reverse Search with `Ctrl+R`

1. Press `Ctrl+R` to start the reverse history search.
2. Type a query to highlight matching entries.
3. Press `Ctrl+R` again to cycle through older matches.
4. Accept the current match with `Tab`, `Esc`, or `Enter` to execute immediately.
5. Cancel the search with `Ctrl+C` or `Backspace` on an empty query.

## Background Bash Commands

The Bash integration can run long commands asynchronously while you continue working with the agent.

### Running in the Background

- Ask VT Code to run a command in the background, or
- Press `Ctrl+B` while a command runs to move it to the background (press twice if your terminal uses tmux with the same prefix).

Background tasks return immediately with an ID. VT Code keeps streaming updates via the BashOutput tool, and tasks are automatically cleaned up when the session ends.

Common backgrounded commands include:

- Build systems (e.g., webpack, vite, make)
- Package managers (npm, yarn, pnpm)
- Test runners (jest, pytest)
- Development servers and other long-running processes

### Waiting for long-running command sessions

For an agent-managed `write_stdin` or `unified_exec` session, use the explicit
`action = "wait"` operation instead of repeatedly polling every few seconds:

```json
{"action":"wait","session_id":"build-1","wait_timeout_seconds":300}
```

The wait deadline is only an observation deadline. If the process is still
running, VT Code returns an in-progress result with the same reusable session
ID; a later explicit wait can continue it. The configured
`timeouts.long_running_command_ceiling_seconds` remains the hard upper bound,
and cancellation still terminates the process. Model-visible output is bounded
to a preview and includes the total byte count, truncation state, exit status,
and `spool_path` when the spool file is open and healthy. An active session may
set `spool_complete` to `false`; that path is a readable partial snapshot. If
the process has exited before draining finishes, VT Code withholds the path,
retains the session, and a later wait can return the completed reference.
For spooled command results, the response contains one preview capped at the
smaller of the requested output budget and 6 KiB. Inspection commands retain
head and tail context; verification and mutation commands retain the tail.
The complete spool file and failure or recovery metadata remain available by
reference without being reread while the response is built.

### Bash Mode with `!`

Prefix input with `!` to run commands directly without agent interpretation:

```bash
! npm test
! git status
! ls -la
```

Bash mode streams the command and its output into the chat, supports backgrounding via `Ctrl+B`, and is ideal for quick shell operations while keeping a shared context with the agent.

## Additional Resources

- [User guide overview](../README.md)
- [Getting started walkthrough](../user-guide/getting-started.md)
