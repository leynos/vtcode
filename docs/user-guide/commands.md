# Command Reference

This guide summarizes common actions and how to invoke them with vtcode. The agent exposes a suite of tools to the LLM; you interact with them via chat. When you ask to search, read, or edit files, the agent chooses an appropriate tool.

## Search

Use `exec_command.cmd` with `rg` or `grep` for flexible shell text search.

The default model-visible tools are `exec_command`, `write_stdin`, and
`apply_patch`. `code_search` is available through the advanced VT Code profile.
It accepts a required literal `query` plus optional `path`, `file_types`,
`result_types`, and `max_results`. Its four result categories are recognized
definitions, exact syntactic usages, literal text, and matching paths. A usage
is a same-spelling syntax occurrence, not a resolved reference. Queries use
literal smart-case. When `truncated` is true, narrow a filter in another call;
the response does not claim an exact repository-wide total.

Use `exec_command` or the specialized ast-grep skill for arbitrary structural
patterns.

## Storage diagnostics

Use `vtcode --version` to print the active config, data, state, cache, runtime,
executable, and legacy roots, as well as the migration marker and report paths.
This is the quickest way to diagnose an XDG override, a sandbox environment,
or a legacy `~/.vtcode` migration without relying on platform-specific guesses.

### Examples

-   Find TODO/FIXME with 2 lines of context in Rust files only:

```
Ask: Search for TODO or FIXME across the repo with 2 lines of context in Rust files.
{
  "cmd": "rg -n -C 2 'TODO|FIXME' -g '*.rs' ."
}
```

-   Literal search for `unsafe {` anywhere (hidden files ignored):

```
{
  "cmd": "rg -n -C 1 -F 'unsafe {' ."
}
```

-   Search JavaScript files for a function name, case-insensitive:

```
{
  "cmd": "rg -n -i 'doSomethingImportant' -g '*.js' ."
}
```

## File operations

-   Inspect files with shell commands through `exec_command.cmd`, such as `rg --files`, `sed -n`, `cat`, `head`, and `tail`.
-   Edit files with `apply_patch`.
-   Continue live shell sessions with `write_stdin`.

## Session resume and forks

VT Code can reopen archived sessions, continue the latest one, or fork a previous session into a new archive.

### Resume the latest or a specific session

```bash
vtcode --continue
vtcode --resume session-123
vtcode --resume          # interactive picker
```

### Fork from an archived session

```bash
vtcode --fork-session session-123
vtcode --fork-session session-123 --session-id bugfix-branch
vtcode --resume session-123 --session-id bugfix-branch
vtcode --continue --session-id followup-branch
```

Notes:

- `--session-id` turns `--resume ...` or `--continue` into a fork instead of an in-place resume.
- `--resume` with no ID plus `--session-id ...` opens the interactive picker and then forks the selected session.
- `--all` expands the picker/search scope across workspaces for resume and fork flows.

### Start a summarized fork

```bash
vtcode --fork-session session-123 --summarize
vtcode --resume session-123 --session-id handoff --summarize
vtcode --resume --session-id handoff --summarize   # interactive picker, summarized fork
```

Summarized forks do not copy the full transcript. VT Code starts the child session from:

- one structured conversation summary
- retained recent real user messages
- the session memory envelope

Normal forks keep the full archived transcript unchanged.

## Quick Actions in Chat Input

VT Code provides several quick actions directly in the chat input for faster workflow:

-   **File Picker (`@`)** — Type `@` anywhere in your input to open the file picker and select files to reference in your message. This allows you to quickly mention files without typing full paths.
-   **Slash Commands (`/`)** — Type `/` at the start of input to access all available slash commands including `/files`, `/stats`, and many more.

## Configuration quick reference

| Action | Command | Result |
| --- | --- | --- |
| Open settings | `/config` (or `/settings`) | Browse categorized settings and effective values |
| Open a section | `/config <path>` | Jump directly to a settings section |
| Reset the active layer | `/config reset` | Show the target file and request confirmation |
| Generate configuration | `vtcode config` | Print a generated configuration document |
| Reset workspace/explicit layer | `vtcode config reset` | Clear one workspace-layer file and reload the stack |
| Reset user layer | `vtcode config reset --global` | Clear the canonical user config only |
| Reset project layer | `vtcode config reset --project` | Clear the current project profile only |

Configuration resets preserve lower-precedence values and secure credentials.
Interactive sessions live-reload safe changes from watched layers; malformed
edits retain the last valid snapshot and show a warning. See the [configuration
guide](../config/config.md) for layer precedence and live-reload details.

### `/code-ide` (VS Code integration)

Use the `/code-ide` slash command to trigger IDE-specific actions from within a VT Code chat session or the integrated terminal. When the VS Code extension is installed:

-   Run `/code-ide` in the VT Code terminal session to synchronize with the sidebar views and refresh context-aware data.
-   Use the **VT Code: Send /code-ide Slash Command** command palette entry or the Quick Actions panel to dispatch the slash command directly to the active VT Code terminal.
-   The command will emit IDE events back to the extension host, keeping the Agent Loop timeline, status indicators, and MCP configuration summaries in sync.

Configure the behaviour under **Settings › Extensions › VT Code**:

-   `vtcode.terminal.autoRunChat` — Automatically run `vtcode chat` when the managed terminal opens.
-   `vtcode.terminal.allowMultipleInstances` — Opt-in to creating new terminal sessions instead of reusing the shared VT Code terminal.
-   `vtcode.agentTimeline.refreshDebounceMs` — Control how quickly the Agent Loop timeline reacts to incoming terminal output.

### Slash-command notes

- Slash commands are skill-backed. Each command routes through a namespaced command skill such as `cmd-status` or `cmd-review`.
- The `/name` form remains the compatibility alias. You can also inspect or execute the same behaviour through `/skills info cmd-name` and `/skills use cmd-name ...`.
- Prompt-oriented slash commands such as `/review`, `/analyse`, and `/command` are shipped as bundled system skills in the release binary.
- To keep the default prompt lean, command skills are not injected into the runtime `## Skills` prompt section; use slash completion or `/skills` discovery when you need them.
- `/resume` opens archived sessions when the current run is idle.
- `/fork` opens the session picker and then lets you choose between a full-copy fork and a summarized fork.
- `/compact` manually compacts the current conversation context immediately. Use `/compact edit-prompt` or `/compact reset-prompt` to manage the saved default prompt for manual compaction requests. On the local fallback path, VT Code keeps a structured summary plus retained user prompts instead of a mixed recent tail.
- `/agent` and `/agents` inspect agent definitions and delegated child runs. `@agent-name` remains a delegated child-agent control. Primary agents are switched from the TUI with `Tab` on an empty idle composer. The active primary agent is shown in the session header badge and influences the session's instructions, model, granular permission policy, and tool access.
- `/agents list` shows all agent definitions with their availability (`mode: primary`, `mode: subagent`, or `mode: all`). `/agents create` scaffolds a new agent definition in `.vtcode/agents/`.
- `/plan` starts or continues the planning workflow. It is a workflow command, not a state selector. Execution agents may also suggest it for demanding or multi-phase tasks; interactive policies confirm the suggestion, while full-auto and skip-confirmations policies accept it automatically. When the plan agent needs a material clarification, the inline interview wizard presents selectable answers and resumes planning with the chosen answer. Use `/plan off` to cancel an active planning workflow without implementing its draft.
- `/checkup` runs configuration diagnostics and suggests reversible optimizations. Use `/checkup [--quick|--full]` (defaults to a full pass); optimizations are confirmed via the selection modal before any config is mutated.

## WebMCP browser bridge

`/webmcp` displays the active-session bridge status, prints a command guide,
and can start the bridge inside an interactive TUI session. Use
`/webmcp help` to print the guide without the status details:

```text
/webmcp
/webmcp help
/webmcp pair http://localhost:5173
/webmcp pair --replace http://localhost:5173
/webmcp tools
/webmcp roots
/webmcp unpair
```

Use `/webmcp pair <origin>` when the browser must submit real agent turns to
this same VT Code session. The printed WebSocket URL and one-time pairing code
belong to that running TUI process.

For the published WebMCP app, use
`https://vtcode.vinhnx.chatgpt.site` for the ChatGPT Site or
`https://vinhnx.github.io` for the GitHub Pages page at
`https://vinhnx.github.io/VTCode/`. The path is not part of the origin. Add
both exact origins to `[webmcp].allowed_origins` when one listener should serve
both pages.

Pair from the two sides in this order:

1. In the VT Code TUI, run `/webmcp pair http://localhost:5173`.
2. In the WebMCP editor, open **Connect to a local VT Code bridge**.
3. Paste the TUI's WebSocket URL and one-time pairing code into the browser,
   then select **Pair with VT Code**.

Keep the TUI running. A URL and code from `vtcode webmcp serve` are for the
headless workspace bridge and cannot receive active-session agent turns.

If `/webmcp pair <origin>` reports that WebMCP is already listening, it prints
the active WebSocket URL, exact browser origin, pairing code, and expiry. A
second configured origin receives a new code while existing sessions stay
connected. To revoke current sessions and replace the pairing, run
`/webmcp pair --replace <origin>` and confirm **Disconnect and re-pair** in the
terminal. `/webmcp unpair` uses the same confirmation and closes the current
browser connections.

The standalone command starts the opt-in authenticated WebSocket bridge for
headless workspace operations:

```bash
vtcode webmcp serve --origin http://localhost:5173
vtcode webmcp pair --origin http://localhost:5173
vtcode webmcp status
vtcode webmcp tools
vtcode webmcp roots
vtcode webmcp unpair
```

The standalone `vtcode webmcp unpair` command cannot revoke an already running
server's in-memory pairing state. It only reports that revocation is owned by
the server process. Stop that process separately with `Ctrl-C` to revoke its
active sessions.

The server binds to loopback and chooses an available port by default. Enter its one-time pairing code in the browser editor. Exact origins are required; remote access must use `--allow-remote --public-url wss://...` behind a TLS-terminating reverse proxy, and direct non-loopback binding is rejected. Browser confirmation cannot authorize a real write: terminal permission or the existing explicit full-auto allowlist remains authoritative. See [WebMCP bridge development](../development/webmcp.md).

For the browser walkthrough, including the WebMCP app, local pairing,
connected drafts, and troubleshooting, see the [WebMCP browser bridge user guide](./webmcp.md).

## Scheduled tasks

Use `vtcode schedule` when the task should survive restarts.

```bash
vtcode schedule create --prompt "check the deployment" --every 10m
vtcode schedule create --prompt "review the nightly build" --cron "0 9 * * 1-5"
vtcode schedule create --reminder "push the release branch" --at "15:00"
vtcode schedule list
vtcode schedule delete 1a2b3c4d
vtcode schedule serve
```

See [Scheduled Tasks](./scheduled-tasks.md) for session reminders, durable daemon behaviour, and service installation details.

## stats (session metrics)

Display current configuration, available tools, and live performance metrics for the running
session. Use `--format` to choose `text`, `json`, or `html` output and `--detailed` to list each
tool.

## schema (runtime tool introspection)

Inspect VT Code's built-in tool schemas at runtime so automation can discover exact tool names and
input parameters without relying on stale docs.

### Usage

```bash
# Full JSON document (default)
vtcode schema tools

# Compact schema descriptions for tighter context windows
vtcode schema tools --mode minimal

# NDJSON output for streaming parsers
vtcode schema tools --format ndjson

# Filter to specific tools
vtcode schema tools --name exec_command --name apply_patch
```

### Options

- `--mode` — `minimal`, `progressive` (default), or `full`
- `--format` — `json` (default) or `ndjson`
- `--name` — repeatable exact tool-name filter

## update (binary updates)

Check for and install binary updates of VT Code from GitHub Releases. Updates are downloaded and verified against checksums for security.

### Usage

```bash
# Check for available updates without installing
vtcode update --check

# Check for updates (same as above, default behaviour)
vtcode update
```

### Options

- `--check` — Check for updates and display release notes without installing
- `--force` — Force update even if already on the latest version

### How it works

1. The command checks the GitHub API for the latest VT Code release
2. It compares the remote version with your current version
3. If a new version is available, it shows release notes and download information
4. Interactive TUI sessions automatically check for updates on launch (short cached interval)
5. Managed installs (Homebrew/cargo/npm) show package-manager-specific update guidance

Standalone releases download the exact platform archive, verify a published SHA-256
checksum when available, safely extract `vtcode`/`vtcode.exe`, and replace the running
binary natively. Binaries built with the former updater must be bootstrapped once with
the native installer before this replacement flow can update them.

### Examples

- Check for updates:
  ```bash
  vtcode update --check
  ```

- Check for updates and show if you're on the latest version:
  ```bash
  vtcode update
  ```

## dependencies

Manage optional VT Code dependencies such as ripgrep and ast-grep.

### Usage

```bash
# Install both optional search tools in one step
vtcode dependencies install search-tools

# Check whether VT Code can resolve the optional search tools
vtcode dependencies status search-tools

# Install ripgrep using a supported system installer
vtcode dependencies install ripgrep

# Install the managed ast-grep binary into the canonical executable directory
vtcode dependencies install ast-grep

# Materialize the bundled ast-grep scaffold in the current workspace
vtcode init

# Run ast-grep rule tests and scan for the current workspace
vtcode check ast-grep

# Check whether VT Code can resolve ast-grep
vtcode dependencies status ast-grep
```

### Notes

- `vtcode deps ...` is a short alias for `vtcode dependencies ...`
- `vtcode dependencies install search-tools` bundles the recommended `ripgrep` + `ast-grep` setup after any install method
- `vtcode dependencies install ripgrep` installs `rg` through a supported system installer and keeps startup non-blocking when you skip it
- `vtcode init` materializes VT Code's bundled ast-grep starter files into the current workspace: `sgconfig.yml`, `rules/`, and `rule-tests/`
- `vtcode check ast-grep` is the first-class replacement for the repo-only `./scripts/check.sh ast-grep` flow
- VT Code does not auto-edit your shell profile; on Linux/BSD, add `export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"` yourself if you want managed binaries available outside VT Code
- On Linux, prefer `ast-grep` over `sg`
- The curl installer includes the search-tools bundle by default; use `--without-search-tools` to skip it

## check

Run built-in repository checks from VT Code.

### Usage

```bash
# Install ast-grep if needed
vtcode dependencies install ast-grep

# Materialize the bundled ast-grep scaffold in the current workspace
vtcode init

# Run ast-grep rule tests and scan
vtcode check ast-grep
```

### Notes

- `vtcode check ast-grep` runs `ast-grep test --config sgconfig.yml` and then `ast-grep scan --config sgconfig.yml`
- The command expects `sgconfig.yml` in the workspace root and points you to `vtcode init` when the scaffold has not been materialized yet

## plugins

Manage [Agent Plugins](agent-plugins.md) — portable packages that bundle Agent Skills and MCP servers under a `plugin.json` manifest.

```bash
# Install a plugin from a git URL or local directory into ~/.agents/plugins
vtcode plugins add https://github.com/example/my-plugin

# List installed plugins with skill and MCP server counts
vtcode plugins list

# Show plugin details
vtcode plugins info my-plugin

# Validate a plugin directory without installing it
vtcode plugins validate ./my-plugin

# Uninstall a plugin
vtcode plugins remove my-plugin
```

### Notes

- `plugins add` clones git URLs with `git clone --depth=1` and requires local directories to contain a valid `plugin.json`
- Plugin MCP servers are exposed as `<plugin>.<server>` providers and connect at session startup

## pods

Manage remote GPU-backed model pods over SSH. See the full feature guide in
[GPU Pod Manager](../features/GPU_POD_MANAGER.md).

### Usage

```bash
# Start a pod-backed model
vtcode pods start --name llama \
  --model meta-llama/Llama-3.1-8B-Instruct \
  --ssh "ssh root@gpu.example.com" \
  --gpu 0:A100 --gpu 1:A100 \
  --gpus 2

# Inspect tracked pods
vtcode pods list

# Stream logs for one model
vtcode pods logs --name llama
```

### Commands

- `vtcode pods start` - Launch a model on the active pod
- `vtcode pods stop` - Stop one tracked model
- `vtcode pods stop-all` - Stop every tracked model on the active pod
- `vtcode pods list` - Show tracked model status
- `vtcode pods logs` - Stream the remote log for a model
- `vtcode pods known-models` - Show compatible and incompatible profiles

## Did You Mean?

When you type an unrecognized `vtcode` subcommand, VT Code suggests the closest match:

```bash
$ vtcode initilize
# Did you mean?
#   vtcode init
```

Suggestions use fuzzy matching and are colorized. This also applies to slash commands inside interactive sessions.

## Continue / Resume

Resume the most recent archived session:

```bash
vtcode --continue
# or in interactive mode:
# /continue
```

This starts a fresh conversation with the context preserved from the last session. Use it to pick up where you left off after closing VT Code.

## Tips

-   The agent respects `.vtcodegitignore` to exclude files from search and I/O.
-   Prefer `exec_command.cmd` with `rg` for fast, focused text searches with glob filters and context.
-   Ask for “N lines of context” when searching to understand usage in-place.
-   Shell commands are filtered by allow/deny lists and can be extended via `VTCODE_<AGENT>_COMMANDS_*` environment variables.
-   Use `vtcode update --check` regularly to stay informed about new features and security updates.
