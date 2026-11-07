# RFC 0003: Headless ACP/CLI runtime

- Status: Proposed; documentation only, not an implementation or benchmark.
- Date: 2026-11-07.
- Codename: `kthxbai`.
- Scope: Delete the terminal user interface (TUI) and make WebMCP opt-in.
- Delivery: [Headless roadmap](../project/diapason-headless-roadmap.md).

## 1. Decision and motivation

Diapason will be an Agent Client Protocol (ACP) and command-line interface
(CLI) agent. Remove the TUI implementation from the codebase altogether. Do not
retain an optional TUI feature, dormant renderer, vendored copy, or replacement
interactive terminal framework. Git history preserves provenance; a UI
extraction project is not a prerequisite for deletion.

Retain WebMCP as a supported, explicitly selected Cargo feature named `webmcp`,
disabled in the default build and default release packaging. A runtime setting
cannot enable a capability that the binary does not contain. Ordinary MCP
client functionality and provider WebSocket transports remain independent.

The expected benefits are a smaller build graph, less UI-specific maintenance,
and a simpler initialization boundary. Runtime memory savings remain a
hypothesis: an unused renderer need not allocate its interactive state. No RAM,
latency, or build-time improvement is claimed without a controlled comparison.

## 2. Inspected baseline

The source baseline is commit `0c54619cb827e48bf09ea3b68b41cd6d2e72d905`. These
are static observations, not execution results.

The root manifest directly depends on `diapason-ui`, `ratatui`, `crossterm`, and
`diapason-webmcp`; its default workspace members include the UI crate. Core
also depends directly on the UI and Ratatui. Its `tui` feature gates only part
of the implementation, and ACP enables core's defaults through its own
dependency edge.[^1] Cargo features are additive, so a root
`--no-default-features` flag alone does not remove that dependency closure.[^2]

The LLM crate has another Ratatui edge: its Ollama pull reporter imports
terminal operations through `ratatui::crossterm`.[^3] Bootstrap also imports UI
panic handling and log capture. ACP already skips interactive terminal probing
and maintenance through the command startup policy; removing that
already-skipped work cannot count as a new ACP speed-up.[^4]

The WebMCP crate owns a browser bridge, not ordinary MCP client integration.
Removing its default dependency must not disable MCP tools or outbound
Responses WebSocket support.[^5]

## 3. Product and compatibility contract

### 3.1. Retained behaviour

Preserve ACP initialization, session creation, streaming, steering,
cancellation, approval, workspace isolation, tool execution, usage reporting,
and durable recovery. Preserve useful non-interactive CLI operations, including
`ask`, `exec`, review, configuration, authentication, and session inspection.
Inventory actual commands before choosing their final disposition.

Keep `ThreadEvent` as the canonical runtime event vocabulary. Retain policy,
sandboxing, file-version checks, prompt preservation, and audit/persistence
boundaries. A UI removal must not silently replace approvals with automatic
permission or discard useful context to obtain a smaller memory reading.

PTY execution is not a TUI. Preserve PTY-backed tools, ANSI sanitization,
protocol image handling, and shell safety where retained capabilities need
them. Plain-text CLI output and optional simple styling are legitimate;
terminal widgets, screen rendering, raw-mode input, and interactive overlays
are not. Do not remove image or parser dependencies merely by association
without checking their non-presentation consumers.

### 3.2. Removed behaviour and migration

Remove TUI chat, overlays, terminal editors, theme selection, animation,
clipboard integration used only by the TUI, palette probing, UI-specific
notification wiring, and their tests, fixtures, benchmarks, and packaging.
Shared application logic moves to an existing suitable non-UI module before its
presentation wrapper disappears; this is not permission to rename the renderer
and keep it.

A no-command invocation must show ordinary usage rather than launch a screen
interface. Retired TUI-only commands and options must produce documented
migration diagnostics, not silently run a different task. A retained alias may
route to a headless operation only when its semantics genuinely match.

Preserve ordinary CLI authentication and explicitly supported line-oriented
input. Non-interactive CLI approval must use an existing authorized mechanism
or fail closed. Never read a confirmation from ACP protocol stdin or display
terminal escape sequences on its stdout. Keep machine-readable output valid and
route diagnostics to stderr or the appropriate protocol error channel.

Document removed configuration fields. For one migration boundary, accept
identified harmless presentation keys with a deprecation warning where that
avoids breaking existing configurations. Reject keys that request removed
behaviour explicitly; do not weaken unknown-key validation globally. Keep
history readable without retaining UI rendering implementations.

### 3.3. WebMCP capability matrix

Default builds must exclude `diapason-webmcp` and dependencies/features used
exclusively by that adapter. Gate its command implementation, listener
construction, configuration activation, and bootstrap wiring together.
Unsupported explicit activation must fail before any provider or listener
starts, with a diagnostic explaining the missing `webmcp` build feature. Help
and capability reporting must agree with the effective build.

An explicit `--features webmcp` build retains the browser bridge and its
security checks. Refactor any bridge dependency on TUI handles into the
existing application event/session boundary. Do not add a parallel event bus.
Compile-time inclusion does not authorize runtime listening: existing explicit
enablement, authentication, workspace controls, and origin restrictions remain.

Do not ban all HTTP server or WebSocket crates by name: another retained
feature may legitimately use them. Check reachability and enabled features from
the shipped executable, not the mere presence of a package in `Cargo.lock`. A
workspace lockfile may still include optional WebMCP packages.

## 4. Implementation boundaries

Remove the UI crate from workspace membership, default membership, dependency
and patch declarations, public re-exports, installation scripts, CI matrices,
examples, and benchmarks. Delete the `tui` feature rather than leaving a no-op
compatibility switch. Remove UI-only dependencies at every introducing edge,
including the LLM progress reporter. Avoid replacing a convenient Ratatui
re-export with an equally unnecessary terminal stack in core.

Separate the headless entrypoint's error reporting and tracing composition from
UI panic hooks. Keep process hardening and useful structured diagnostics.
Remove default desktop notifications where they are presentation-only; retain
an optional non-TUI use only with an identified consumer and explicit scope.

The production agent path must not depend on a no-op UI object. Move narrowly
required shared types and behaviour inward, then delete the adapter. Reuse the
existing contracts work in issues #42, #49, #50, and #86 rather than inventing
another agent runtime.[^6] Full completion of that programme is not required
before independent manifest and presentation deletions can land.

This decision supersedes retaining a reusable in-repository TUI for issues #79
and #80. Record that scope change without automatically closing useful non-UI
lifecycle work. Reconcile the observability proposal's references to retained
TUI logging with this decision; preserve its privacy and bounded export
requirements.[^7] Do not mutate those issues in this documentation PR.

## 5. Verification and performance evidence

The functional acceptance matrix covers default and `--no-default-features`
ACP/CLI builds, explicit WebMCP builds, and the supported all-features build.
No supported combination may resurrect the TUI. Test the supported target
matrix, including non-Linux builds; validate the actual declared compiler
minimum and lockfile rather than copying stale prose.

Add two distinct ratchets: a source/import check proving that no TUI
implementation remains, and a resolved dependency-graph check rooted at the
shipped executable. Explicitly allow historical documentation references;
searching for the word "TUI" is not a source-code proof. Ensure default build
commands do not select the optional WebMCP workspace member independently.

Exercise the real binary over pipes with a deterministic provider fixture:
handshake, prompt, streamed commentary, tool call, approval denial,
cancellation, recovery, and EOF. Assert valid stdout, no terminal probing or
clipboard access, no accidental listener, and unchanged semantic event traces.
Reuse existing correctness fixtures and rebase onto their integrated fixes;
open PR descriptions are not evidence that those fixes are already present.

Measure clean and warm builds, compiler/linker peak memory, artifact size, ACP
ready time, session creation, and idle/busy proportional set size (PSS). Keep
compiler, target, profile, allocator, runtime workers, and fixtures fixed.
Record compiler-cache state and target-directory size separately. Never bypass
the host compile-admission queue or clear a shared cache to simulate a cold
build. Publish raw results even when RSS barely changes.

## 6. Delivery, risks, and alternatives

Use the linked roadmap for review-sized slices. Land behavioural contract
fixtures before deleting mixed application/presentation paths. Keep default
WebMCP exclusion and complete TUI deletion independently reviewable, but the
programme is not complete until both hold.

A feature-gated TUI is rejected because it retains the maintenance burden and
contradicts the product decision. A separate UI extraction is not part of this
programme. Deleting all terminal/PTY support is rejected because it breaks
coding tools. A new universal headless framework is unnecessary.

This is an intentional user-facing removal. Publish a release-specific
migration guide and reconcile active docs, generated references, completions,
agent instructions, notices, and packaging. Historical records may describe the
old product. An emergency rollback may revert a defective implementation
commit; it does not establish a maintained dual-mode product.

## 7. References

[^1]: Baseline manifests: [root](../../Cargo.toml),
    [core](../../crates/codegen/diapason-core/Cargo.toml), and
    [ACP](../../crates/codegen/diapason-acp/Cargo.toml).
[^2]: [Cargo feature unification](https://doc.rust-lang.org/cargo/reference/features.html#feature-unification).
[^3]: [LLM manifest](../../crates/codegen/diapason-llm/Cargo.toml) and
    [Ollama pull reporter](../../crates/codegen/diapason-llm/src/providers/ollama/pull.rs).
[^4]: [Bootstrap](../../src/main.rs) and
    [command startup policy](../../src/startup/mod.rs).
[^5]: [WebMCP manifest](../../crates/codegen/diapason-webmcp/Cargo.toml) and
    [ACP runtime contract](../development/acp-runtime.md).
[^6]: [Application boundary #42](https://github.com/leynos/vtcode/issues/42),
    [result projections #49](https://github.com/leynos/vtcode/issues/49),
    [turn execution #50](https://github.com/leynos/vtcode/issues/50), and
    [headless providers #86](https://github.com/leynos/vtcode/issues/86).
[^7]: [TUI reuse #79](https://github.com/leynos/vtcode/issues/79),
    [TUI lifecycle #80](https://github.com/leynos/vtcode/issues/80), and
    [observability RFC](rfc-0002-diapason-observability.md).
