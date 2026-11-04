# Diapason WebMCP App

This is a browser code editor with a safe in-memory fallback. In a real
Diapason session, the browser sends reviewed proposals to Diapason. Diapason
performs the terminal-approved file changes. The browser never writes to the
filesystem directly.

## Start the WebMCP app

Run these commands from the Diapason repository root:

```sh
cd apps/webmcp
./start.sh
```

Open <http://localhost:5173>. This starts fallback mode; no Diapason process is
required. Because no bridge is paired yet, **Settings** opens automatically.
Close it to use the fallback editor, or pair a Diapason bridge there. Press `?`
after closing Settings for the short in-app guide and shortcuts.

For a real workspace and agent turns, use the recommended active mode:

```sh
cd apps/webmcp
./start.sh --active --workspace /absolute/path/to/project
```

The launcher starts Vite and an interactive Diapason session. In the Diapason
TUI, run:

```text
/webmcp pair http://localhost:5173
```

Open **Settings** in the browser and paste the printed WebSocket URL and
pairing code. Use the values from this same TUI session. Do not start a separate
`webmcp serve` process for active agent turns.

For workspace browsing without agent turns, use:

```sh
./start.sh --headless --workspace /absolute/path/to/project
```

Paste the URL and code printed by the bridge into **Settings**. Headless mode
is useful for reading and proposing changes; terminal approval and DIAPASON
TURN require active mode.

Use `--port 5174` if port `5173` is already in use. Use the same port in the
browser URL and `/webmcp pair` command.

## Pair a self-hosted client

Diapason does not currently publish a hosted browser app. Run the app locally
or serve a build from a host you control. The page derives its pairing origin
from `window.location.origin`; configure that exact origin in the bridge.

When the local page is open in a browser on the same machine as the active
Diapason TUI, run:

```text
/webmcp pair http://localhost:5173
```

If the browser uses another host or port, pair its exact origin instead. Use
`--replace` only when you want to revoke current sessions and issue a fresh
code:

```text
/webmcp pair --replace http://localhost:5173
```

Paste the newest WebSocket URL and one-time code into **Settings**. The browser
origin must exactly match the allowlist; a mismatch can appear as a generic
**WebSocket connection failed** message.

The printed `ws://127.0.0.1:<port>/webmcp` URL is reachable only from a browser
that can reach the same machine. A remote or sandboxed in-app browser needs a
TLS-terminating reverse proxy and a reachable `wss://` endpoint.

### Use the Chrome origin trial

Chrome's
[WebMCP origin trial](https://developer.chrome.com/blog/ai-webmcp-origin-trial)
is an optional way to test a self-hosted page without relying on the testing
flag. Tokens are origin-specific; request one for each origin where you serve
the app. Configure `VITE_WEBMCP_ORIGIN_TRIAL_TOKENS` at build time. No token is
committed to source control.

For local development, use a token registered for the local origin or enable
`chrome://flags/#enable-webmcp-testing` instead. A production token will not
authorize `http://localhost:5173`.

## Test browser WebMCP tools

The public editor is usable without a bridge, so it is the easiest target for a
real browser-agent check. Use a supported WebMCP browser with a valid
origin-trial token or enable `chrome://flags/#enable-webmcp-testing`, relaunch
Chrome, and open the deployed editor. Chrome's
[WebMCP documentation](https://developer.chrome.com/docs/ai/webmcp) links to
the Model Context Tool Inspector, which can list tools, execute a tool with
JSON input, and display structured output or errors.

WebMCP is page-scoped: keep the editor open in a browser tab or webview while
the agent uses it. Current Chrome also gates the API on an origin-isolated
document and a `tools` Permissions Policy that allows the browsing context. The
editor reports those observed prerequisites in `get_editor_state` as
`webmcp_context`; the fallback remains usable when a prerequisite is missing.
The optional Diapason bridge is a separate authenticated adapter and does not
make WebMCP calls headlessly.

### Record real-client evidence

The header's **Evidence** control records the calls made through the page's
registered WebMCP callbacks. It does not simulate a client or infer tool use
from clicks. To capture a run:

1. Open **Evidence**, choose the client you are about to use, and select
   **Start new run** before opening the inspector or sending a prompt.
2. In Chrome 150.0.7861.0 or later, enable
   `chrome://flags/#enable-webmcp-testing`, relaunch Chrome, open the deployed
   page, and install/use the
   [WebMCP Model Context Tool Inspector](https://chromewebstore.google.com/detail/webmcp-model-context-tool/gbpdfapgefenggkahomfgkhfehlcenpd).
   In ChatGPT, open the deployed page in the in-app browser when WebMCP is
   available and use the **ChatGPT in-app browser** label.
3. Exercise the sequence below through that client. Keep the page open; the
   WebMCP API is browsing-context scoped.
4. Return to **Evidence**, select **Copy JSON** or **Download JSON**, and keep
   the export with the client screenshot or screen recording. The export is
   sanitized and intentionally omits file contents, diffs, prompts, pairing
   codes, session tokens, and other sensitive fields.

The selected client label is an attestation by the person recording the run;
the JSON itself proves which page callbacks ran, not the identity of the
external client. Treat the export and the screenshot/video as one evidence
bundle. Do not count **Run self-check** as a real-client pass: it exercises the
fallback editor, not an external WebMCP client.

Run this sequence and record the selected tool, arguments, result, and visible
page effect:

1. Ask, “What files are available in this workspace?” and expect
   `list_project_files`.
2. Ask, “Find the file that defines the greeting and open it in the editor,” and
   expect `search_code` followed by `open_file` using the returned path.
3. Ask, “Change Hello to Hi in the greeting and prepare the draft for my
   review,” and expect `read_file`, `stage_text_edit`, then `review_draft`
   using the digest returned by `read_file`.
4. Ask, “Show me the current draft diff, then open the changes panel,” and
   expect `review_draft` followed by `open_panel` with `changes`.
5. Execute a long-file or many-match request and confirm the response stays
   bounded and reports truncation.
6. Ask for the current editor state and verify it reports the workflow state,
   recommended next tools, and `webmcp_context` prerequisites.
7. Try a stale digest, an ambiguous replacement, an empty search term, and an
   invalid panel. Confirm each error explains how to recover.
8. Confirm no exposed browser tool can approve, apply, or revert a filesystem
   change; those operations remain behind Diapason's separate approval boundary.

The repository's deterministic version of these checks is in
`evals/webmcp-evals.ts` and runs as part of `bun run test`. It validates the
tool contract and failure behavior; the Chrome inspector pass is still needed
to measure probabilistic tool selection.

## Edit and apply a change

1. Open a file in **EXPLORER** and edit it. A `*` marks a dirty draft.
2. Select **Review changes** or press `Cmd/Ctrl+S`.
3. In **CHANGES**, inspect the browser preview and stage it. Active mode sends
   the proposal ID; Diapason rechecks the files and generates the authoritative
   diff for the turn.
4. In **DIAPASON**, enter a prompt and select **Request Diapason turn**, or
   press `Cmd/Ctrl+Enter` in the prompt.
5. Approve requests in the Diapason terminal.
6. Select **Reload** after Diapason finishes.

In fallback mode, use **Approve patch**, **Apply approved patch**, **Run
checks**, and **Revert last change**. Fallback changes remain in memory and are
not written to disk.

## Pairing and reconnecting

- `/webmcp` shows the active listener and available commands in the TUI.
- `/webmcp pair --replace http://localhost:5173` disconnects the current bridge
  after confirmation and creates a new pairing.
- `/webmcp unpair` disconnects the active bridge.
- Pairing codes are one-time and expire after five minutes. A restarted bridge
  needs a new URL and code.
- If the session is unauthorized after an idle period, pair again with the
  newest values.

The browser preserves fallback drafts, tabs, selection, and explorer state
across refreshes for the current Vite app instance. It does not store pairing
codes or session tokens. Restarting Vite clears the fallback workspace.

## Useful controls

| Shortcut           | Action                                      |
| ------------------ | ------------------------------------------- |
| `Cmd/Ctrl+Shift+P` | Open the action palette                     |
| `Cmd/Ctrl+,`       | Open Settings                               |
| `Cmd/Ctrl+K`       | Focus the file filter                       |
| `Cmd/Ctrl+S`       | Review the current draft                    |
| `Cmd/Ctrl+Enter`   | Request a Diapason turn                     |
| `?`                | Open help when focus is not in a form field |
| `Esc`              | Close the active popup                      |

Long file lists, editor content, diffs, and terminal output have independent
scroll areas. Files are loaded when opened, so the explorer does not flatten
the entire workspace into the editor.

## Troubleshooting

### Port `5173` is busy

Reuse the existing WebMCP app at <http://localhost:5173>, or start another
instance:

```sh
./start.sh --port 5174
```

Then use `http://localhost:5174` consistently in the browser and pairing
command.

### WebSocket connection failed

Make sure the bridge or active Diapason session is still running. Paste the
newest WebSocket URL and one-time code from that same process. Its origin must
exactly match the browser origin; `localhost` and `127.0.0.1` are different
origins. Run `/webmcp pair <exact-browser-origin>` to issue a code without
revoking the other session; use `--replace` only when intentionally revoking
it, then paste the newly printed values.

### The TUI prints no URL or pairing code

Run the WebMCP app from the current Diapason checkout, stop old Diapason
processes, and pair again:

```sh
cd /path/to/diapason
cargo run --locked -- chat
```

Then enter `/webmcp pair http://localhost:5173` in that TUI.

### `serve` is an unknown subcommand

Use `./start.sh --headless --workspace /absolute/path/to/project`, or run the
command from a current Diapason checkout with
`cargo run --locked -- webmcp serve ...`.

## Test the WebMCP app

From `apps/webmcp`:

```sh
bun install --frozen-lockfile
bun run typecheck
bun run test
bun run build
```

See the [WebMCP browser bridge user guide](../../docs/user-guide/webmcp.md) for
the full protocol, configuration, and security model.
