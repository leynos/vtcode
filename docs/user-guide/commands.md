# Command Reference

This guide summarizes common actions and how to invoke them with diapason. The
agent exposes a suite of tools to the LLM; you interact with them via chat.
When you ask to search, read, or edit files, the agent chooses an appropriate
tool.

## Search

Use `exec_command.cmd` with `rg` or `grep` for flexible shell text search.

The default model-visible tools are `exec_command`, `write_stdin`, and
`apply_patch`. `code_search` is available through the advanced Diapason
profile. It accepts a required literal `query` plus optional `path`,
`file_types`, `result_types`, and `max_results`. Its four result categories are
recognised definitions, exact syntactic usages, literal text, and matching
paths. A usage is a same-spelling syntax occurrence, not a resolved reference.
Queries use literal smart-case. When `truncated` is true, narrow a filter in
another call; the response does not claim an exact repository-wide total.

Use `exec_command` or the specialised ast-grep skill for arbitrary structural
patterns.

## Storage diagnostics

Use `diapason --version` to print the active config, data, state, cache,
runtime, and executable roots. This is the quickest way to diagnose an XDG
override or a sandbox environment without relying on platform-specific guesses.
Diapason does not read or migrate the historical `~/.diapason/` or `.vtcode/`
directories.

### Examples

- Find TODO/FIXME with 2 lines of context in Rust files only:

```
Ask: Search for TODO or FIXME across the repo with 2 lines of context in Rust files.
{
  "cmd": "rg -n -C 2 'TODO|FIXME' -g '*.rs' ."
}
```

- Literal search for `unsafe {` anywhere (hidden files ignored):

```
{
  "cmd": "rg -n -C 1 -F 'unsafe {' ."
}
```

- Search JavaScript files for a function name, case-insensitive:

```
{
  "cmd": "rg -n -i 'doSomethingImportant' -g '*.js' ."
}
```

## File operations

- Inspect files with shell commands through `exec_command.cmd`, such as
    `rg --files`, `sed -n`, `cat`, `head`, and `tail`.
- Edit files with `apply_patch`.
- Continue live shell sessions with `write_stdin`.

## Session resume and forks

Diapason can reopen archived sessions, continue the latest one, or fork a
previous session into a new archive.

### Resume the latest or a specific session

```bash
diapason --continue
diapason --resume session-123
diapason --resume          # interactive picker
```

### Fork from an archived session

```bash
diapason --fork-session session-123
diapason --fork-session session-123 --session-id bugfix-branch
diapason --resume session-123 --session-id bugfix-branch
diapason --continue --session-id followup-branch
```

Notes:

- `--session-id` turns `--resume ...` or `--continue` into a fork instead of an
  in-place resume.
- `--resume` with no ID plus `--session-id ...` opens the interactive picker
  and then forks the selected session.
- `--all` expands the picker/search scope across workspaces for resume and fork
  flows.

### Start a summarized fork

```bash
diapason --fork-session session-123 --summarize
diapason --resume session-123 --session-id handoff --summarize
diapason --resume --session-id handoff --summarize   # interactive picker, summarized fork
```

Summarized forks do not copy the full transcript. Diapason starts the child
session from:

- one structured conversation summary
- retained recent real user messages
- the session memory envelope

Normal forks keep the full archived transcript unchanged.

## Quick Actions in Chat Input

Diapason provides several quick actions directly in the chat input for faster
workflow:

- **File Picker (`@`)** — Type `@` anywhere in your input to open the file
    picker and select files to reference in your message. This allows you to
    quickly mention files without typing full paths.
- **Slash Commands (`/`)** — Type `/` at the start of input to access all
    available slash commands including `/files`, `/stats`, and many more.

## Configuration quick reference

| Action                         | Command                           | Result                                              |
| ------------------------------ | --------------------------------- | --------------------------------------------------- |
| Open settings                  | `/config` (or `/settings`)        | Browse categorized settings and effective values    |
| Open a section                 | `/config <path>`                  | Jump directly to a settings section                 |
| Reset the active layer         | `/config reset`                   | Show the target file and request confirmation       |
| Generate configuration         | `diapason config`                 | Print a generated configuration document            |
| Reset workspace/explicit layer | `diapason config reset`           | Clear one workspace-layer file and reload the stack |
| Reset user layer               | `diapason config reset --global`  | Clear the canonical user config only                |
| Reset project layer            | `diapason config reset --project` | Clear the current project profile only              |

Configuration resets preserve lower-precedence values and secure credentials.
Interactive sessions live-reload safe changes from watched layers; malformed
edits retain the last valid snapshot and show a warning. See the
[configuration guide](../config/config.md) for layer precedence and live-reload
details.

### `/code-ide` (VS Code integration)

Use the `/code-ide` slash command to trigger IDE-specific actions from within a
Diapason chat session or the integrated terminal. When the VS Code extension is
installed:

- Run `/code-ide` in the Diapason terminal session to synchronize with the
    sidebar views and refresh context-aware data.
- Use the **Diapason: Send /code-ide Slash Command** command palette entry or
    the Quick Actions panel to dispatch the slash command directly to the
    active Diapason terminal.
- The command will emit IDE events back to the extension host, keeping the
    Agent Loop timeline, status indicators, and MCP configuration summaries in
    sync.

Configure the behaviour under **Settings › Extensions › Diapason**:

- `diapason.terminal.autoRunChat` — Automatically run `diapason chat` when the
    managed terminal opens.
- `diapason.terminal.allowMultipleInstances` — Opt-in to creating new terminal
    sessions instead of reusing the shared Diapason terminal.
- `diapason.agentTimeline.refreshDebounceMs` — Control how quickly the Agent
    Loop timeline reacts to incoming terminal output.

### Slash-command notes

- Slash commands are skill-backed. Each command routes through a namespaced
  command skill such as `cmd-status` or `cmd-review`.
- The `/name` form remains the compatibility alias. You can also inspect or
  execute the same behavior through `/skills info cmd-name` and
  `/skills use cmd-name ...`.
- Prompt-oriented slash commands such as `/review`, `/analyze`, and `/command`
  are shipped as bundled system skills in the release binary.
- To keep the default prompt lean, command skills are not injected into the
  runtime `## Skills` prompt section; use slash completion or `/skills`
  discovery when you need them.
- `/resume` opens archived sessions when the current run is idle.
- `/fork` opens the session picker and then lets you choose between a full-copy
  fork and a summarized fork.
- `/compact` manually compacts the current conversation context immediately. Use
  `/compact edit-prompt` or `/compact reset-prompt` to manage the saved default
  prompt for manual compaction requests. On the local fallback path, Diapason
  keeps a structured summary plus retained user prompts instead of a mixed
  recent tail.
- `/agent` and `/agents` inspect agent definitions and delegated child runs.
  `@agent-name` remains a delegated child-agent control. Primary agents are
  switched from the TUI with `Tab` on an empty idle composer. The active
  primary agent is shown in the session header badge and influences the
  session's instructions, model, granular permission policy, and tool access.
- `/agents list` shows all agent definitions with their availability
  (`mode: primary`, `mode: subagent`, or `mode: all`). `/agents create`
  scaffolds a new agent definition in `.diapason/agents/`.
- `/plan` starts or continues the planning workflow. It is a workflow command,
  not a state selector. Execution agents may also suggest it for demanding or
  multi-phase tasks; interactive policies confirm the suggestion, while
  full-auto and skip-confirmations policies accept it automatically. When the
  plan agent needs a material clarification, the inline interview wizard
  presents selectable answers and resumes planning with the chosen answer. Use
  `/plan off` to cancel an active planning workflow without implementing its
  draft.
- `/checkup` runs configuration diagnostics and suggests reversible
  optimizations. Use `/checkup [--quick|--full]` (defaults to a full pass);
  optimizations are confirmed via the selection modal before any config is
  mutated.

## WebMCP browser bridge

`/webmcp` displays the active-session bridge status, prints a command guide,
and can start the bridge inside an interactive TUI session. Use `/webmcp help`
to print the guide without the status details:

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
this same Diapason session. The printed WebSocket URL and one-time pairing code
belong to that running TUI process.

For a self-hosted WebMCP app, pair the exact browser origin shown by the
address bar, such as `http://localhost:5173`. Configure that same origin in
`[webmcp].allowed_origins`. both pages.

Pair from the two sides in this order:

1. In the Diapason TUI, run `/webmcp pair http://localhost:5173`.
2. In the WebMCP editor, open **Connect to a local Diapason bridge**.
3. Paste the TUI's WebSocket URL and one-time pairing code into the browser,
   then select **Pair with Diapason**.

Keep the TUI running. A URL and code from `diapason webmcp serve` are for the
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
diapason webmcp serve --origin http://localhost:5173
diapason webmcp pair --origin http://localhost:5173
diapason webmcp status
diapason webmcp tools
diapason webmcp roots
diapason webmcp unpair
```

The standalone `diapason webmcp unpair` command cannot revoke an already
running server's in-memory pairing state. It only reports that revocation is
owned by the server process. Stop that process separately with `Ctrl-C` to
revoke its active sessions.

The server binds to loopback and chooses an available port by default. Enter
its one-time pairing code in the browser editor. Exact origins are required;
remote access must use `--allow-remote --public-url wss://...` behind a
TLS-terminating reverse proxy, and direct non-loopback binding is rejected.
Browser confirmation cannot authorize a real write: terminal permission or the
existing explicit full-auto allowlist remains authoritative. See
[WebMCP bridge development](../development/webmcp.md).

For the browser walkthrough, including the WebMCP app, local pairing, connected
drafts, and troubleshooting, see the
[WebMCP browser bridge user guide](./webmcp.md).

## Scheduled tasks

Use `diapason schedule` when the task should survive restarts.

```bash
diapason schedule create --prompt "check the deployment" --every 10m
diapason schedule create --prompt "review the nightly build" --cron "0 9 * * 1-5"
diapason schedule create --reminder "push the release branch" --at "15:00"
diapason schedule list
diapason schedule delete 1a2b3c4d
diapason schedule serve
```

See [Scheduled Tasks](./scheduled-tasks.md) for session reminders, durable
daemon behavior, and service installation details.

## stats (session metrics)

Display current configuration, available tools, and live performance metrics
for the running session. Use `--format` to choose `text`, `json`, or `html`
output and `--detailed` to list each tool.

## schema (runtime tool introspection)

Inspect Diapason's built-in tool schemas at runtime so automation can discover
exact tool names and input parameters without relying on stale docs.

### Usage

```bash
# Full JSON document (default)
diapason schema tools

# Compact schema descriptions for tighter context windows
diapason schema tools --mode minimal

# NDJSON output for streaming parsers
diapason schema tools --format ndjson

# Filter to specific tools
diapason schema tools --name exec_command --name apply_patch
```

### Options

- `--mode` — `minimal`, `progressive` (default), or `full`
- `--format` — `json` (default) or `ndjson`
- `--name` — repeatable exact tool-name filter

## update (binary updates)

Check for and install binary updates of Diapason from GitHub Releases. Updates
are downloaded and verified against checksums for security.

### Usage

```bash
# Check for available updates without installing
diapason update --check

# Check for updates (same as above, default behavior)
diapason update
```

### Options

- `--check` — Check for updates and display release notes without installing
- `--force` — Force update even if already on the latest version

### How it works

1. The command checks the GitHub API for the latest Diapason release
2. It compares the remote version with your current version
3. If a new version is available, it shows release notes and download
   information
4. Interactive TUI sessions automatically check for updates on launch (short
   cached interval)
5. Managed installs (Homebrew/cargo/npm) show package-manager-specific update
   guidance

Standalone releases download the exact platform archive, verify a published
SHA-256 checksum when available, safely extract `diapason`/`diapason.exe`, and
replace the running binary natively. Binaries built with the former updater
must be bootstrapped once with the native installer before this replacement
flow can update them.

### Examples

- Check for updates:

  ```bash
  diapason update --check
  ```

- Check for updates and show if you're on the latest version:

  ```bash
  diapason update
  ```

## dependencies

Manage optional Diapason dependencies such as ripgrep and ast-grep.

### Usage

```bash
# Install both optional search tools in one step
diapason dependencies install search-tools

# Check whether Diapason can resolve the optional search tools
diapason dependencies status search-tools

# Install ripgrep using a supported system installer
diapason dependencies install ripgrep

# Install the managed ast-grep binary into the canonical executable directory
diapason dependencies install ast-grep

# Materialize the bundled ast-grep scaffold in the current workspace
diapason init

# Run ast-grep rule tests and scan for the current workspace
diapason check ast-grep

# Check whether Diapason can resolve ast-grep
diapason dependencies status ast-grep
```

### Notes

- `diapason deps ...` is a short alias for `diapason dependencies ...`
- `diapason dependencies install search-tools` bundles the recommended
  `ripgrep` + `ast-grep` setup after any install method
- `diapason dependencies install ripgrep` installs `rg` through a supported
  system installer and keeps startup non-blocking when you skip it
- `diapason init` materializes Diapason's bundled ast-grep starter files into
  the current workspace: `sgconfig.yml`, `rules/`, and `rule-tests/`
- `diapason check ast-grep` is the first-class replacement for the repo-only
  `./scripts/check.sh ast-grep` flow
- Diapason does not auto-edit your shell profile; on Linux/BSD, add
  `export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"` yourself if you want
  managed binaries available outside Diapason
- On Linux, prefer `ast-grep` over `sg`
- The curl installer includes the search-tools bundle by default; use
  `--without-search-tools` to skip it

## check

Run built-in repository checks from Diapason.

### Usage

```bash
# Install ast-grep if needed
diapason dependencies install ast-grep

# Materialize the bundled ast-grep scaffold in the current workspace
diapason init

# Run ast-grep rule tests and scan
diapason check ast-grep
```

### Notes

- `diapason check ast-grep` runs `ast-grep test --config sgconfig.yml` and then
  `ast-grep scan --config sgconfig.yml`
- The command expects `sgconfig.yml` in the workspace root and points you to
  `diapason init` when the scaffold has not been materialized yet

## plugins

Manage [Agent Plugins](agent-plugins.md) — portable packages that bundle Agent
Skills and MCP servers under a `plugin.json` manifest.

```bash
# Install a plugin from a git URL or local directory into ~/.agents/plugins
diapason plugins add https://github.com/example/my-plugin

# List installed plugins with skill and MCP server counts
diapason plugins list

# Show plugin details
diapason plugins info my-plugin

# Validate a plugin directory without installing it
diapason plugins validate ./my-plugin

# Uninstall a plugin
diapason plugins remove my-plugin
```

### Notes

- `plugins add` clones git URLs with `git clone --depth=1` and requires local
  directories to contain a valid `plugin.json`
- Plugin MCP servers are exposed as `<plugin>.<server>` providers and connect
  at session startup

## pods

Manage remote GPU-backed model pods over SSH. See the full feature guide in
[GPU Pod Manager](../features/GPU_POD_MANAGER.md).

### Usage

```bash
# Start a pod-backed model
diapason pods start --name llama \
  --model meta-llama/Llama-3.1-8B-Instruct \
  --ssh "ssh root@gpu.example.com" \
  --gpu 0:A100 --gpu 1:A100 \
  --gpus 2

# Inspect tracked pods
diapason pods list

# Stream logs for one model
diapason pods logs --name llama
```

### Commands

- `diapason pods start` - Launch a model on the active pod
- `diapason pods stop` - Stop one tracked model
- `diapason pods stop-all` - Stop every tracked model on the active pod
- `diapason pods list` - Show tracked model status
- `diapason pods logs` - Stream the remote log for a model
- `diapason pods known-models` - Show compatible and incompatible profiles

## Did You Mean?

When you type an unrecognized `diapason` subcommand, Diapason suggests the
closest match:

```bash
$ diapason initilize
# Did you mean?
#   diapason init
```

Suggestions use fuzzy matching and are colorized. This also applies to slash
commands inside interactive sessions.

## Continue / Resume

Resume the most recent archived session:

```bash
diapason --continue
# or in interactive mode:
# /continue
```

This starts a fresh conversation with the context preserved from the last
session. Use it to pick up where you left off after closing Diapason.

## Tips

- The agent respects `.diapasongitignore` to exclude files from search and I/O.
- Prefer `exec_command.cmd` with `rg` for fast, focused text searches with
    glob filters and context.
- Ask for “N lines of context” when searching to understand usage in-place.
- Shell commands are filtered by allow/deny lists and can be extended via
    `DIAPASON_<AGENT>_COMMANDS_*` environment variables.
- Use `diapason update --check` regularly to stay informed about new features
    and security updates.
