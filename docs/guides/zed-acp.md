# Zed Agent Client Protocol Integration

VT Code adopts [ACP (Agent Client Protocol) by Zed](https://agentclientprotocol.com/). 

It took the reference from the official Zed implementations
([`zed-industries/claude-code-acp`](https://github.com/zed-industries/claude-code-acp),
[`cola-io/codex-acp`](https://github.com/cola-io/codex-acp)) and follows the
[Goose ACP client guidance](https://block.github.io/goose/docs/guides/acp-clients/). Use the steps
below to configure, launch, and validate the integration end to end.

## Setup overview

1. Build VT Code (`target/debug` is fine for local development; `target/release` is recommended for regular editor workflows).
2. Enable the ACP bridge in `vtcode.toml` or via environment overrides.
3. Wire either the VT Code binary or a small wrapper script into Zed's `settings.json` under `agent_servers`.
4. Start an external agent session in Zed and confirm ACP logs report healthy traffic.

## Prerequisites

- Rust toolchain pinned by `rust-toolchain.toml`.
- VT Code configuration with provider, model, and credentials.
- Zed `v0.201` or later with the Agent Client Protocol feature flag enabled.
- An ACP client that advertises the `fs.read_text_file` capability so VT Code can proxy
  `read_file` requests. If the handshake omits it, the bridge keeps the tool disabled and reports a
  reasoning notice.

## Build VT Code

```bash
cargo build --release
```

Record the resulting binary path (`target/release/vtcode`) or add it to your `PATH`.

For local repository development, this is also fine:

```bash
cargo build -p vtcode
```

That produces `target/debug/vtcode`, which is usually the simplest binary to point Zed at while iterating on ACP fixes.

## Configure VT Code for ACP

Open your `vtcode.toml` (project-local copy or the default in the repo root) and enable the bridge:

```toml
[acp]
enabled = true

    [acp.audit]
    # Optional; disabled by default. Entries contain metadata and hashes only.
    enabled = false
    path = "~/.vtcode/audit/acp-tools.jsonl"

    [acp.zed]
    enabled = true
    transport = "stdio"
    workspace_trust = "full_auto"

        [acp.zed.tools]
        read_file = true
        list_files = true
```

Environment overrides provide the same control surface:

| Variable | Purpose |
| --- | --- |
| `VT_ACP_ENABLED` | Toggles the global ACP bridge. |
| `VT_ACP_ZED_ENABLED` | Enables the Zed transport. |
| `VT_ACP_ZED_TOOLS_READ_FILE_ENABLED` | Switches the `read_file` tool forwarding on or off. |
| `VT_ACP_ZED_TOOLS_LIST_FILES_ENABLED` | Controls whether the `list_files` bridge is available. |
| `VT_ACP_ZED_WORKSPACE_TRUST` | Forces the workspace trust mode (`full_auto` by default, `tools_policy` optional). |

When targeting models that cannot call tools (for example `openai/gpt-oss-20b:free` on OpenRouter),
disable the `read_file` bridge. VT Code emits reasoning notices and structured logs when it detects
models without function calling and automatically downgrades to plain completions.

### Session working directories

The ACP client selects the workspace for each new session through the absolute
`cwd` in `session/new`. VT Code canonicalizes that directory before creating
the session and rejects missing, relative, or non-directory paths. It does not
silently fall back to the directory from which the ACP server was launched.

The canonical session workspace scopes archive metadata, system instructions,
prompt templates, lifecycle hooks, compaction artefacts, local and ACP tools,
MCP sandboxing, and subagents. Multiple sessions on one ACP connection may use
different workspaces; their tool registries and mutable harness state are kept
separate.

### MCP providers in ACP sessions

ACP sessions initialise the enabled providers from the effective session MCP configuration before
the first prompt. This makes the providers' direct MCP proxy tools available in the initial model
tool catalogue rather than waiting for a later discovery step. Configure the providers and the
global MCP switch in the [`[mcp]` configuration](mcp-integration.md).

Direct MCP proxy tools remain subject to the same MCP provider allowlists and security checks as
interactive sessions. The selected ACP primary agent's tool permissions also apply, so a provider
or tool blocked by either policy is not exposed to, or executable by, the ACP session.

### Lifecycle hooks in ACP

The canonical `[hooks.lifecycle]` configuration is also used by ACP sessions;
no ACP-specific hook schema is required. Hooks are scoped to each ACP session
and follow this ordering: `SessionStart` once for `session/new` or resumed
sessions, `UserPromptSubmit` before provider work, `PreToolUse` and
`PermissionRequest` before a tool runs, `PostToolUse` after its result,
`PreCompact` before automatic compaction, and `Stop` before ACP reports a final
turn. A blocking prompt is refused; a blocking stop hook feeds its reason back
into the same turn and continues until the hook allows the draft or the turn
is cancelled.

For `PermissionRequest`, hook `updatedInput` is applied to the tool call. ACP
does not persist hook permission scopes or apply `permission_updates`; it logs
an explicit warning when those fields are returned. An `interrupt` decision
fails the tool safely.

`SessionEnd` is emitted when the ACP connection actually closes (with a bounded
shutdown wait), not after every prompt. Notification hooks are not invented for
ACP protocol messages; they run only for real VT Code notification events. The
current ACP subagent controller does not expose child lifecycle callbacks, so
ACP does not currently emit `SubagentStart` or `SubagentStop`.

MCP connections are scoped to the session that declares them. A subagent does not implicitly inherit
its parent's MCP connections; declare the required MCP servers in the subagent's own configuration.

## Manual smoke test

Run the bridge directly to ensure it starts cleanly:

```bash
./target/release/vtcode acp
```

Add `--config /absolute/path/to/vtcode.toml` if the configuration lives outside the default lookup
locations. You can also mirror Codex CLI behaviour with inline overrides such as
`--config agent.provider="openai"` when launching the bridge. Successful startup leaves the process
waiting on stdio; stop it with `Ctrl+C`.

For a source checkout, test the same binary Zed will use:

```bash
./target/debug/vtcode --config /absolute/path/to/vtcode.toml acp
```

If you see a crash or panic here, fix that before debugging Zed itself. Zed can only report
"server shut down unexpectedly" after the process exits; it will not explain VT Code panics for you.

## Register VT Code in Zed

Edit `settings.json` (Command Palette → `zed: open settings`) and add a custom agent entry:

```jsonc
{
    "agent_servers": {
        "vtcode": {
            "command": "/absolute/path/to/vtcode",
            "args": ["acp"],
            "env": {
                "VT_ACP_ENABLED": "1",
                "VT_ACP_ZED_ENABLED": "1",
                "RUST_LOG": "info"
            },
            "cwd": "/workspace/containing/vtcode"
        }
    }
}
```

- Rename the key from `vtcode` if you want a different label in Zed.
- Trim `command` to just `"vtcode"` when the binary is on `PATH`.
- Add CLI flags such as `--config` or `--log-level debug` to `args` if required.

### Recommended development wrapper

When running VT Code from a local repo checkout, prefer a wrapper script over pointing Zed at an
installed copy in `~/.local/bin`. A wrapper keeps the binary path, config path, provider overrides,
and working directory in one place.

Example:

```sh
#!/bin/sh
set -eu

REPO="/absolute/path/to/VTCode"
cd "$REPO"

exec "$REPO/target/debug/vtcode" \
  --config "$REPO/vtcode.toml" \
  acp
```

Then register that wrapper in Zed:

```jsonc
{
    "agent_servers": {
        "vtcode": {
            "type": "custom",
            "command": "/absolute/path/to/VTCode/scripts/zed-vtcode-acp.sh",
            "args": [],
            "env": {}
        }
    }
}
```

This avoids a common failure mode where Zed launches an older installed binary that does not match
the source tree you are editing.

## Use it inside Zed

1. Open the agent panel (`Cmd-?` on macOS) and choose **External Agent**.
2. Select the `vtcode` entry you added. Zed spawns VT Code and bridges ACP over stdio.
3. Chat normally. Mention files (`@src/lib.rs`) or attach buffers. When enabled, the `read_file`
   tool proxies to Zed's `fs.readTextFile` capability and streams results back into the turn, while
   `list_files` uses VT Code's workspace indexer for directory exploration.

## Package VT Code as a Zed Agent Server Extension

When you are ready to distribute VT Code to other Zed users, wrap the ACP bridge inside an Agent
Server Extension. Extensions bundle both metadata and platform-specific binaries so users can install
VT Code from Zed's marketplace without touching `settings.json`. This repository ships a ready-to-edit
manifest at `extensions/zed-extension/extension.toml`; customize it for each release and publish the directory as
the Zed extension package.

### Extension manifest layout

Add the VT Code agent definition under the `[agent_servers]` table in `extension.toml`. The copy in
`extensions/zed-extension/extension.toml` uses the latest published macOS artifacts as a baseline (note the required top-level metadata):

```toml
[agent_servers.vtcode]
name = "VT Code"
icon = "icons/vtcode.svg"            # Optional, 16x16 monochrome SVG recommended

[agent_servers.vtcode.env]
VT_ACP_ENABLED = "1"
VT_ACP_ZED_ENABLED = "1"

[agent_servers.vtcode.targets.darwin-aarch64]
archive = "https://github.com/vinhnx/vtcode/releases/download/0.133.21/vtcode-darwin-aarch64.tar.gz"
cmd = "./vtcode"
args = ["acp"]
sha256 = "replace-with-real-sha256"

[agent_servers.vtcode.targets.darwin-x86_64]
archive = "https://github.com/vinhnx/vtcode/releases/download/0.133.21/vtcode-darwin-x86_64.tar.gz"
cmd = "./vtcode"
args = ["acp"]
sha256 = "replace-with-real-sha256"

[agent_servers.vtcode.targets.linux-x86_64]
archive = "https://github.com/vinhnx/vtcode/releases/download/0.133.21/vtcode-linux-x86_64.tar.gz"
cmd = "./vtcode"
args = ["acp"]
sha256 = "replace-with-real-sha256"

[agent_servers.vtcode.targets.windows-x86_64]
archive = "https://github.com/vinhnx/vtcode/releases/download/0.133.21/vtcode-windows-x86_64.zip"
cmd = "./vtcode.exe"
args = ["acp"]
sha256 = "replace-with-real-sha256"
```

- `name` controls the label shown in Zed menus.
- `schema_version = 1`, `id`, `name`, `version`, and `display_name` live at the top level.
- `id` must be globally unique (reverse-domain style is recommended).
- Each `{os}-{arch}` target block supplies a download URL, the command to launch, and optional
  arguments. The example above reuses the `acp` entry-point so the extension behaves like the manual
  setup described earlier in this guide.
- The checked-in manifest currently declares macOS targets (`darwin-aarch64`, `darwin-x86_64`).
  Add Linux or Windows target tables when you start publishing those builds.
- Set `sha256` to the checksum of the published archive to harden supply-chain trust. The release
  script (`./scripts/release.sh`) regenerates these values automatically after the binaries are
  built; you can also run `shasum -a 256 <archive>` on macOS/Linux or
  `certutil -hashfile <archive> SHA256` on Windows to verify them manually.
- Provide an optional `[agent_servers.vtcode.env]` section when you need to carry configuration such
  as ACP toggles or provider credentials. Avoid hard-coding secrets; rely on Zed's environment
  overlays or documented setup steps instead.

### Building and publishing the archives

1. Produce release builds for every platform you intend to support (see `scripts/` for cross-compiling
   helpers). Bundle the artifacts as `.tar.gz` or `.zip` archives that include the `vtcode` binary at
   the root, plus any support files (for example `vtcode.toml.example`).
2. Create a GitHub release and upload each archive. Copy the asset URLs into `extensions/zed-extension/extension.toml`.
3. Run `./scripts/release.sh` to execute the automated release flow. It rebuilds the binaries,
   uploads release assets, and rewrites `extensions/zed-extension/extension.toml` with fresh SHA-256 checksums
   for every archive that exists in `dist/`.
4. Confirm each target you ship is represented in the manifest; add new target tables as you
   introduce additional builds.
5. Commit the extension assets alongside `extension.toml`. Keep the directory structure stable so
   future updates can reuse the same icon and metadata.

### Local testing workflow

1. Use the Command Palette (`Cmd-Shift-P`) → `zed: install dev extension` to load the local
   workspace as an extension.
2. Open the Agent panel, pick the **VT Code** entry, and confirm the download succeeds on your
   platform.
3. Exercise ACP capabilities (tool calls, workspace prompts, cancellation) while watching Zed’s ACP
   logs to ensure the packaged binary behaves the same as your development build.
4. Repeat on every supported platform (macOS, Linux, Windows) before publishing the extension to the
   marketplace, verifying the correct archive is fetched and the shell wrapper behaves as expected.

## Troubleshooting launch failures

Use this order:

1. Run the exact ACP command manually in a terminal.
2. Confirm the same binary path works outside Zed before changing editor settings.
3. Check `~/Library/Logs/Zed/Zed.log` for `agent_servers::acp` lines.
4. On macOS, check `~/Library/Logs/DiagnosticReports/` if the process is killed before it prints errors.

Common cases:

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `server shut down unexpectedly` in Zed, but no useful ACP stderr | VT Code crashed very early | Run `vtcode acp` manually and inspect the generated crash report |
| `zsh:1: no such file or directory: .local/bin/vtcode` | Relative `command` path in Zed | Use an absolute path or a wrapper script |
| `Failed to create workspace .vtcode directory at /.vtcode` | Zed launched VT Code with `/` as cwd | Set `cwd` correctly or use a wrapper that runs `cd /path/to/repo` first |
| `Authentication not found for OpenAI` during ACP startup | ACP is booting with the wrong config or provider | Pass `--config /absolute/path/to/vtcode.toml` or use a wrapper script |
| Process is killed immediately on macOS | Local binary signature/install issue | Rebuild locally and point Zed at `target/debug/vtcode` or a wrapper |

### Keep protocol alignment

- Review the [ACP initialization contract](https://agentclientprotocol.com/protocol/initialization) when updating handshake fields so `agent_capabilities`, `agent_info`, and auth methods stay in sync with the spec. 
- Cross-check `NewSession` behaviour with Zed’s expectations outlined in the [session setup flow](https://agentclientprotocol.com/workflows/session/new) before changing session lifecycle code.
- Tool routing (for example `fs.readTextFile`) should continue to follow the [tools guidance](https://agentclientprotocol.com/protocol/tools) so capability negotiation and permission prompts remain interoperable.

## Runtime behaviour

### Storage boundaries

ACP continuity uses three independent stores:

1. **Compaction history/artifacts** – Stored under
   `<workspace>/.vtcode/history/` and controlled by `[context.dynamic] enabled`
   and `persist_history`. These artifacts support summarisation and
   working-memory recovery, including the session memory envelope. They are not
   a complete resumable session and are not read by ACP `session/load`.
2. **Durable session archive** – Stored under `$VT_SESSION_DIR`, or the
   resolved VT Code configuration directory's `sessions/`, and controlled by
   `[history].persistence` and `max_bytes`. These JSON snapshots are used by
   ACP `session/load` and checkpoint user messages, assistant tool requests,
   tool results, completed responses, and incomplete turns.
   The archive metadata also preserves opaque ACP `_meta` values received on
   `session/new` and `session/prompt` as `acp_meta`; prompt values merge into
   the session map and replace earlier values with the same key.
3. **ACP security tool audit** – Written to `[acp.audit].path` only when
   `[acp.audit].enabled = true`. This append-only JSONL contains invocation
   metadata, status, timing, and hashes. It does not contain conversation,
   request, or result bodies and cannot be used for resume or compaction.

These stores are independent: disabling one does not disable the others. In
particular, `context.dynamic` settings do not control durable session archives,
and `[history]` settings do not control ACP audit output.

- **Session management** – ACP advertises and implements `session/list` and
  `session/resume` while retaining the legacy `session/load` method. With
  durable history enabled, `session/list` discovers durable archives and, when
  the client supplies `cwd`, returns only archives belonging to that workspace.
  An ACP client can pass a returned session ID to either `session/load` or
  `session/resume`; both restore that archive by exact session ID, including
  after a VT Code process or editor restart. Set `history.persistence = "none"`
  to disable durable archives and discovery; sessions then remain process-local.
- **Sub-agent delegation** – When ACP sub-agents are enabled, the session exposes the canonical
  `agent` tool. Its actions are `spawn`, `spawn_subprocess`, `send_input`, `wait`, `resume`, and
  `close`. Before delegation, VT Code synchronises the current parent ACP session identity and
  parent context with the sub-agent controller. The active primary agent's tool permissions still
  apply; delegation does not bypass its allowed or disallowed tools.
- **Provider request capacity** – For a custom provider with `max_in_flight_requests` configured,
  one request slot is reserved for the parent ACP request and child concurrency is capped at the
  remaining slots. A limit of `1` therefore disables ACP sub-agents so the parent retains its slot.
- **Provider diagnostics** – Debug logs record queue depth, active and maximum provider permits,
  retry count and disposition, time to first output, generation duration, output tokens, and
  tokens per second. Providers that support streaming report first-output latency from before
  stream establishment, so response-header delay counts toward the first-output timeout. Buffered
  responses label their time-to-first-token observation as the full buffered response latency.
  VT Code does not currently gate provider requests with a circuit
  breaker, so provider events report `circuit_breaker_state = "not_configured"`; the tool and MCP
  circuit breakers are separate. HTTP 408, 429, 500, 502, 503, and 504 failures are eligible for
  the configured bounded retry policy before output becomes visible.
- **Tool-call budgets** – ACP applies `agent.harness.max_tool_calls_per_turn` and the optional
  `agent.harness.max_tool_calls_per_session` to local tool execution. Set either value to `0` to
  disable that cap. The separate `tools.max_tool_loops` provider-loop guard also accepts `0` for
  unlimited turns.
- **Context ingestion** – URIs such as `file://`, `zed://`, or `zed-fs://` resolve through Zed's
  `fs.readTextFile` capability, following Goose's recommended structure.
- **Embedded resources** – Inline text is wrapped in `<context>` blocks so the model can separate
  supporting material from primary instructions. Binary data is acknowledged but omitted from the
  prompt payload.
- **Streaming updates** – Token deltas and reasoning updates arrive via `session/update`
  notifications, keeping Zed's UI responsive during generation. Tool-enabled turns remain
  streamed: VT Code waits for the provider's completed tool-call arguments, executes the tool,
  appends its result, and opens the next provider stream. A configured `Stop` lifecycle hook is the
  deliberate exception; VT Code buffers that draft so the hook can accept or block it before any
  text becomes visible. A stream failure after visible output is never replayed and checkpoints an
  incomplete assistant response instead.
- **Provider reasoning** – When a provider response exposes reasoning alongside tool calls, VT Code
  sends it to ACP as an `AgentThoughtChunk` before executing those tools. Exposed reasoning on the
  final provider response is sent as an `AgentThoughtChunk` as well. Debug logs distinguish
  `Sending provider reasoning to ACP client` from `Provider response did not include exposed reasoning
  for ACP`; they record metadata only and never the reasoning content.
- **Plan tracking** – Every prompt emits an ACP plan describing analysis, optional context gathering,
  and final response drafting. VT Code updates each entry as it progresses so Zed can visualise the
  bridge's workflow in real time.
- **Tool execution** – The `read_file` tool forwards to Zed when enabled. The `list_files` tool
  uses VT Code's local workspace access, mirroring the CLI experience. When the model lacks
  function calling or the tool toggle is disabled, VT Code surfaces a reasoning notice and skips the
  invocation. Pending `apply_patch` calls include the decoded patch text as ACP tool-call content so
  the client can show the proposed edit before execution. Paths supplied by tools are normalised
  against the trusted workspace so relative segments stay inside the project before the request
  reaches the client.
- **File versions and patch preconditions** – `read_file` results include a full-file
  `content_hash` in `sha256:<64 lower-hex>` form, including partial and paged reads. A model can
  carry that value into `apply_patch.expected_content_hash` when a patch has exactly one
  pre-existing source file. A mismatch is write-free and reports the expected and current hashes,
  bounded failed-anchor context, whether the patch can be safely rebased, and an explicit reread or
  regeneration action. Omitting the precondition remains supported and retains VT Code's internal
  external-mutation protection.
- **Identical patch guard** – If the rendered post-patch bytes already equal the initial bytes, the
  first identical request succeeds with an explanation and the second succeeds with a warning not
  to retry it. The third is a non-retryable structured tool error. Changing the parsed patch,
  canonical path set, or file versions resets this streak; changing only the `input`/`patch` alias,
  base64 wrapper, path order, or line-ending envelope does not. These structured results and errors
  are forwarded through ACP and retained in tool execution history.
- **Tool policy compatibility** – VT Code advertises the current core tool suite
  through ACP when the model supports function calling, including `exec_command`,
  `write_stdin`, `apply_patch`, and advanced `code_search` where enabled. The
  bridge evaluates each request against the workspace's tool-policy settings
  before executing commands locally, ensuring shell access and editing tools
  behave the same as in the native CLI. Policy defaults and overrides defined
  under `[tools]` in `vtcode.toml` apply to ACP sessions just like the CLI.
- **Policy persistence** – Auto-approved tool prompts in ACP mode (for example shell execution in a
  non-interactive environment) are stored in the workspace policy file so subsequent runs reuse the
  remembered decision instead of prompting on every invocation.
- **Workspace trust** – On first launch the bridge records the workspace as fully trusted (matching
  the default `workspace_trust = "full_auto"`). Existing full auto entries are respected, and
  previously trusted workspaces aren't downgraded automatically.
- **Permission prompts** – The bridge requests explicit approval in Zed before each `read_file`
  invocation so you can confirm access to sensitive paths. If Zed cannot surface the prompt, the tool
  call is cancelled instead of executing without consent.
- **Cancellations** – When you stop a turn in Zed, VT Code stops streaming tokens, aborts pending
  tool execution with cancellation notices, and responds to the prompt with the ACP `cancelled`
  stop reason so no extra output appears after you abort the run.
- **Graceful degradation** – Unsupported payloads (images, binary blobs) emit structured
  placeholders rather than failing the prompt turn.

### Capability negotiation and safety

- VT Code inspects the Zed initialization payload before enabling each tool. When
  `fs.read_text_file` is absent, the bridge refuses to expose `read_file` and inserts a
  reasoning notice so transcripts document the downgrade.
- Every filesystem request is paired with a `session/request_permission` call so the user
  approves or rejects path access inside Zed. Denials and cancellations are surfaced as ACP
  tool updates rather than silent failures.
- Arguments are validated as absolute workspace paths prior to invoking the client method,
  preventing accidental traversal outside the project boundary.

### Telemetry and auditing

- Plan updates enumerate analysis, context gathering, and response drafting so audit trails
  show exactly how a turn progressed.
- Cancellation signals from Zed immediately cut off streaming, mark pending tool calls as
  cancelled, and end the turn with `StopReason::Cancelled`, providing a clean timeline in the
  transcript.
- Downgrades (such as models without tool calling) are emitted as explicit reasoning notices
  so reviewers can understand why a turn completed without filesystem access.

## Debugging and verification

| Symptom | Resolution |
| --- | --- |
| `Only the stdio transport is supported` | Ensure `transport = "stdio"` in `vtcode.toml`. |
| Empty responses in Zed | Confirm ACP env vars are present in the `env` map and that ACP is enabled in `vtcode.toml`. |
| `read_file` returns placeholders | Validate the referenced URI is accessible from Zed's workspace. |
| Tool calls report "Unsupported tool" | Disable the tool bridge or switch to a model that supports function calling. VT Code emits a reasoning notice when the downgrade occurs. |
| Missing thought traces in Zed | Enable debug logging and inspect whether VT Code reports `Sending provider reasoning to ACP client` or `Provider response did not include exposed reasoning for ACP`. The latter means the provider response exposed no reasoning; verify that the selected provider/model and API format return reasoning. Neither message logs reasoning content. |
| Sessions cancel unexpectedly | Inspect VT Code logs (and Zed's ACP logs) for cancellations triggered by the client. |

## Next steps

- Forward additional tools (for example MCP proxies) when the workspace requires editing or shell
  access directly from the editor.
- Advertise ACP command palettes once Zed surfaces richer UI affordances.
- File integration issues upstream so the bridge can track protocol or client changes.
