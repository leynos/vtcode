# Diapason WebMCP app

This Vite project is the maintained browser app for Diapason's WebMCP bridge.
It is a real CodeMirror 6 editor for a Diapason workspace and supports editable
drafts, syntax highlighting, tabs, line numbers, dirty state, unified diff
review, checks, prompt requests, and backend events.

The app-owned runtime, tests, evals, and Vite configuration use strict
TypeScript. Bun is the supported package manager and script runner. The
JavaScript files shown in the fallback workspace are intentional virtual sample
files.

For the click-by-click setup and fallback/connected workflow, see the
[WebMCP app guide](GUIDE.md) or the
[WebMCP browser bridge user guide](../../docs/user-guide/webmcp.md).

## Self-hosted browser app

Diapason does not currently publish a hosted WebMCP browser app. Run the Vite
app locally with `bun install` and `bun dev`, then pair the exact origin
printed by the browser (normally `http://localhost:5173`) with the running
Diapason bridge. For a public deployment, host the built app yourself and add
its exact origin to the bridge allowlist. See the
[deployment reference](../../docs/reference/webmcp.md) for configuration and
verification guidance.

The browser never writes to the local filesystem. With no bridge, the page
starts in a deterministic `InMemoryBackend`; edits, proposals, checks, and
reverts are confined to page memory. When paired with `diapason webmcp serve`,
the same UI uses an authenticated WebSocket backend. Diapason verifies base
digests, owns the authoritative diff, applies terminal or full-auto policy, and
validates current files before reverts.

The interface is intentionally IDE-shaped. The explorer keeps directories
collapsed, lists file metadata without loading every file, and reads content
only when a file is opened. The bottom panel separates `TERMINAL`, `CHANGES`,
and `DIAPASON` output so large files and long command output stay inside their
own scroll areas.

## Run locally

From the Diapason repository root:

```sh
cd apps/webmcp
./start.sh
```

The launcher installs dependencies if needed and starts Vite at
`http://localhost:5173`. The Vite base is relative, so the generated site also
works beneath a GitHub Pages project path.

On startup, an unpaired browser opens **Settings** so you can paste the
Diapason WebSocket URL and one-time pairing code. Close it to use fallback mode.

The browser preserves the fallback project, drafts, open tabs, and selected
file across a browser refresh. A new Vite app instance clears that browser
state. Real bridge credentials are never stored, so a refreshed page must be
paired again before it can access a real workspace.

If your terminal is already in `apps/webmcp`, skip the `cd` command.

Start a connected workflow with the same launcher:

```sh
./start.sh --headless --workspace /absolute/path/to/project
./start.sh --active --workspace /absolute/path/to/project
```

Use `--headless` for workspace-only access. Use `--active` for **DIAPASON
TURN**; when the TUI is ready, run `/webmcp pair http://localhost:5173` and
paste its WebSocket URL and pairing code into the browser. Add `--port 5174`
when port `5173` is already occupied. See [GUIDE.md](GUIDE.md) for the complete
workflow.

Build and test the WebMCP app with:

```sh
bun install --frozen-lockfile
bun run typecheck
bun run test
bun run build
```

## Capture real-client evidence

Open **Evidence** in the header, select **Chrome WebMCP Tool Inspector** or
**ChatGPT in-app browser**, and choose **Start new run** before the external
client uses the page. Run the browser-agent sequence in [GUIDE.md](GUIDE.md),
then use **Copy JSON** or **Download JSON**. Keep the sanitized export with a
tool-inspector screenshot or screen recording; it records page-side discovery,
tool calls, bounded metadata, errors, and editor state, but omits file
contents, diffs, prompts, pairing codes, tokens, and other sensitive fields.
The selected client label is a human attestation, so review it together with
the client capture. **Run self-check** is not a substitute for a real
external-client run.

### Optional Chrome origin trial

The Vite build supports opt-in
[WebMCP origin-trial](https://developer.chrome.com/blog/ai-webmcp-origin-trial)
tokens without storing them in the repository. Tokens are bound to exact
origins. For a single-origin build, register the exact origin where you host
the app, then set:

```sh
VITE_WEBMCP_ORIGIN_TRIAL_TOKEN='<token for the deployment origin>' bun run build
```

When one artifact is served from multiple origins, provide origin-trial tokens
as a comma- or newline-separated `VITE_WEBMCP_ORIGIN_TRIAL_TOKENS` value. The
browser ignores a token issued for a different origin. If no token is set, use
the Chrome WebMCP testing flag or the normal in-memory fallback. A separately
published ChatGPT Site artifact must receive equivalent build variables from
its own publisher; the Pages workflow deploys only GitHub Pages. Never reuse a
production token for `localhost`; tokens are time-limited and origin-specific.

## Pair with Diapason

Pairing is a two-step handoff:

1. **Diapason TUI:** run `/webmcp pair http://localhost:5173` in the same
   interactive session that should receive browser prompts.
2. **Web app:** open **Settings → Connect or re-pair a Diapason bridge**, paste
   the WebSocket URL and one-time pairing code printed by the TUI, and select
   **Pair with Diapason**.

In the TUI, `/webmcp` shows the current bridge details and available command
arguments. Use `/webmcp help` to show only the command guide.

The TUI prints the values in this order:

```text
WebSocket: ws://127.0.0.1:<port>/webmcp
Browser origin: <exact-browser-origin>
Pairing code: <one-time-code>
```

Keep the TUI running. Do not use a URL or code from an old session or from a
separate headless `webmcp serve` process when you want **DIAPASON TURN**.

If the TUI says WebMCP is already listening, it prints the active WebSocket
URL, pairing code, and expiry. To disconnect that browser and issue a fresh
pairing, run `/webmcp pair --replace http://localhost:5173`, then confirm
**Disconnect and re-pair**. `/webmcp unpair` also asks for confirmation before
disconnecting without starting a replacement bridge.

When `[webmcp].allowed_origins` contains both published origins, running
`/webmcp pair <the-other-origin>` while the bridge is already listening issues
a new one-time code without revoking existing sessions. Use `--replace` only
when the current browser sessions should be revoked.

The app's **Settings** dialog contains the workspace path, browser origin,
generated active-session and workspace-only setup commands, WebSocket URL, and
pairing code. Enter the absolute workspace path there and copy the setup
command you need. The browser cannot launch Diapason itself; run the copied
command in a terminal, then enter the printed URL and code under **Connect or
re-pair a Diapason bridge**.

After pairing, the **Bridge status** section mirrors the terminal-owned
workspace, runtime mode, listener, pairing lease, and request limits. The
workspace path and WebSocket URL are remembered for the current Vite app
instance, but pairing codes and session tokens are never persisted. Use the VT
Code TUI to change origins, roots, or policy, then pair again.

### Workspace-only pairing

For workspace inspection without agent turns, start the headless bridge from
the repository checkout with an explicit origin allowlist:

```sh
cd /path/to/diapason
cargo run --locked -- webmcp serve \
  --origin http://localhost:5173 \
  --allowed-root /absolute/path/to/workspace
```

Enter the newest printed `ws://.../webmcp` address and matching one-time
pairing code in the editor's Settings dialog. Keep the bridge terminal running.
A restart can change the port and always invalidates the previous code. The
pairing code and returned session token are held only in JavaScript memory;
they are not put in query parameters, browser storage, or logs. The connected
browser sends a small authenticated heartbeat so an open session can remain
paired while it is idle. If the browser is suspended longer than the session
lease, restart the standalone bridge and enter its new URL and code. In the
active-session workflow, run `/webmcp pair ...` again in the TUI instead. The
terminal remains the write authority. A default server started without explicit
full-auto permission can inspect and stage proposals but rejects filesystem
mutation requests; checks also require `exec_command` (or `*`) in the explicit
full-auto allowlist.

If an installed `diapason` says `unrecognized subcommand 'serve'`, update it or
run `cargo run --locked -- webmcp serve ...` from the source checkout. The
configured origin must match the page exactly: `localhost:5173` and
`127.0.0.1:5173` are different origins.

The standalone `serve` command is headless: it does not open an interactive
terminal approval prompt or execute agent turns. For the complete
active-session workflow, see the
[WebMCP browser bridge user guide](../../docs/user-guide/webmcp.md).

## Editor workflow

1. Filter the workspace with **Filter files**, expand a directory, and open a
   file. Only the selected file is loaded into the editor.
2. Edit the file in its draft buffer. Open another file from the explorer or
   tabs without losing the draft.
3. Use `Cmd/Ctrl+S` or **Review changes** to create a client-side unified diff.
4. Select **CHANGES** in the bottom panel and review the diff. **Approve
   patch** sends structured changes with their `sha256:` base digests. In
   fallback mode this is an in-memory approval; in connected mode it requests
   Diapason/terminal approval.
5. Apply only after the backend accepts the proposal. Stale files and
   external changes fail closed.
6. Select **TERMINAL** to inspect activity and checks, or **DIAPASON** to
   compose an agent turn.

After **Stage for Diapason turn**, the active-session editor opens the **VT
CODE** panel and focuses the prompt. Press `Cmd/Ctrl+Enter` in the prompt to
request the turn without clicking the button.

The prompt composer attaches the reviewed diff to a Diapason turn request. The
static fallback and the standalone headless bridge do not execute an agent
turn: the editor reports that limitation in the response panel. The active TUI
bridge started with `/webmcp pair <origin>` is the runtime path for a real
model response.

Reload compares a clean draft with the latest backend snapshot. A dirty buffer
is marked as an external-change conflict instead of being silently overwritten.
File paths, content sizes, proposal counts, commands, and WebSocket frames are
bounded by the backend. The file filter narrows the explorer by name or path;
it is not a full-content search.

## Keyboard shortcuts

| Shortcut           | Action                                                   |
| ------------------ | -------------------------------------------------------- |
| `Cmd/Ctrl+Shift+P` | Open the quick-action command palette.                   |
| `Cmd/Ctrl+,`       | Open Settings.                                           |
| `?`                | Open or close the help popup when you are not typing.    |
| `Cmd/Ctrl+K`       | Focus the explorer file filter.                          |
| `Cmd/Ctrl+S`       | Review the current draft instead of writing immediately. |
| `Cmd/Ctrl+Enter`   | Request a Diapason turn from the prompt composer.        |
| `Esc`              | Close the active popup.                                  |

The **Actions** and **?** buttons in the header provide the same entry points.
The command palette filters actions as you type and disables actions that are
not available for the current editor state or backend.

## WebMCP browser API

When the browser exposes `document.modelContext.registerTool`, the page
registers eight standard WebMCP tools: bounded file listing, current-file
reading, code search, editor-state inspection, file opening, exact text editing
of a browser draft, draft review, and panel navigation. Browser-only edits
change the draft buffer and never approve, apply, or revert filesystem changes.
Each tool has a display title, JSON Schema input, read-only/untrusted-content
annotations, an abort-aware callback, and a 1,500-character result budget.
Truncated file, diff, and collection results carry explicit metadata so an
agent can decide whether to narrow the request. The registration listens for
`toolchange` and can be unregistered with its `AbortSignal`. `get_editor_state`
also reports a small workflow state and recommended next tools so an agent can
recover its place after a refresh or a multi-step request. The page
intentionally does not pass `exposedTo`: this WebMCP app has no trusted
cross-origin embedder.

This is the browser-agent direction described by WebMCP: a browser-integrated
or in-page agent discovers tools registered by the page and invokes them in the
page's execution context. The normal UI remains usable when that API is
unavailable. Browser WebMCP tools do not bypass the Diapason WebSocket pairing
or terminal approval boundary.

## WebMCP evaluation workflow

The deterministic eval corpus in `evals/webmcp-evals.ts` covers direct
requests, open-ended search-and-open requests, the review journey, and invalid
tool arguments. Each case records the user goal, initial editor state,
boundaries, expected UI effects, success criteria, and recovery guidance.
Runtime errors name the next safe action, such as rereading a stale file or
choosing one of the allowed panels. `test/webmcp-evals.test.ts` checks that
every expected tool exists, metadata stays within Chrome's recommended
discoverability budgets, and browser tools never gain a write or revert
authority. Run it with the rest of the WebMCP app tests:

```sh
bun run test
```

For a real browser-agent pass, use a supported WebMCP browser with either a
valid origin-trial token for the page origin or Chrome 150.0.7861.0 or later
with `chrome://flags/#enable-webmcp-testing` enabled, relaunch Chrome, and open
this page. Use the
[WebMCP Model Context Tool Inspector](https://chromewebstore.google.com/detail/webmcp-model-context-tool/gbpdfapgefenggkahomfgkhfehlcenpd)
to inspect `getTools()` and manually execute the `list_project_files`,
`search_code`, `read_file`, `open_file`, `stage_text_edit`, `review_draft`, and
`open_panel` calls. Then try these natural-language prompts:

1. "What files are available in this workspace?"
2. "Find the file that defines the greeting and open it in the editor."
3. "Change Hello to Hi in the greeting and prepare the draft for my review."
4. "Show me the current draft diff, then open the changes panel."

Record the selected tool, arguments, returned structure, and whether the UI
changed as expected. Also verify that a long file or search result reports
truncation, and that no browser tool can approve, apply, or revert a filesystem
change. This manual model pass is intentionally separate from the deterministic
tests: model tool selection is probabilistic and the repository does not bundle
a browser-agent model runner.

The Diapason connection is a separate authenticated bridge. The browser sends
workspace, patch, check, and turn requests over that WebSocket; Diapason sends
responses and canonical runtime events back. WebMCP does not define that remote
Diapason protocol, so this WebMCP app does not claim that the WebSocket itself
is the WebMCP API.

## Build and host the browser app

The repository workflow checks the Vite app. To build it for a host you
control, run `bun install --frozen-lockfile` and `bun run build`, then serve
`dist/` from your chosen origin. Pair that exact browser origin with the
Diapason bridge.

The WebMCP app is covered by the repository's
[MIT OR Apache-2.0 license](../../LICENSE).
