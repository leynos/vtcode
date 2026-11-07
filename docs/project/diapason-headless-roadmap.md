# Headless ACP/CLI roadmap

This roadmap delivers
[RFC 0003](../rfcs/rfc-0003-diapason-headless-runtime.md), codename `kthxbai`.
Status: proposed; every implementation task remains open. Phases are ideas to
validate, steps are workstreams, and numbered tasks have observable acceptance
criteria. Merging this plan does not remove code.

## 1. Establish the headless product contract

Idea: a recorded command/capability inventory and executable fixtures allow
presentation deletion without losing useful agent behaviour.

### 1.1. Define retained and removed surfaces

- [ ] 1.1.1. Audit commands, configuration, exports, and consumers.
  - Classify each as retained headless behaviour, removable presentation, or
    shared logic that must move before deletion.
  - Success: every TUI entrypoint and non-UI consumer has an explicit outcome;
    PTYs, shell safety, MCP tools, and protocol images remain accounted for.
  - See [RFC sections 2-4](../rfcs/rfc-0003-diapason-headless-runtime.md).

- [ ] 1.1.2. Record the compatibility and programme reconciliation decision.
  - Define no-command behaviour, removed-option errors, configuration warnings,
    approval handling, and WebMCP feature-disabled diagnostics.
  - Reconcile #79/#80 and the observability roadmap's TUI retention assumptions;
    do not block deletion on extracting a replacement UI for another consumer.
  - Success: an accepted decision and migration matrix contain no retained TUI
    feature and no silent permission or command-semantic changes.
  - Requires 1.1.1.

### 1.2. Capture the production-path baseline

- [ ] 1.2.1. Add headless acceptance fixtures before changing behaviour.
  - Use the real ACP/CLI binary with deterministic local providers and pipes.
  - Cover streaming, tools, approvals, cancellation, recovery, and clean stdout.
  - Success: the fixture records semantic traces and identifies existing
    failures separately from regressions; no test-only agent loop substitutes.
  - Requires 1.1.1.

- [ ] 1.2.2. Capture build, dependency, and runtime resource baselines.
  - Record commit, lockfile, toolchain, target, profile, feature graph, workers,
    allocator, compiler-cache state, artifact size, PSS, and startup timings.
  - Success: reproducible raw evidence distinguishes executable size, private
    memory, and shared pages without assuming a large RAM win.
  - Reuse the [performance workflow](../development/performance.md).

## 2. Remove presentation dependencies and gate WebMCP

Idea: moving only genuinely shared behaviour inward makes deletion possible
without creating a parallel runtime or a no-op renderer.

### 2.1. Prepare headless composition

- [ ] 2.1.1. Move required application behaviour out of presentation modules.
  - Reuse canonical `ThreadEvent` contracts and the narrow #42/#49/#50 seams.
  - Replace UI-owned bootstrap errors, logging, progress, and approvals with
    existing headless mechanisms; preserve security and persistence boundaries.
  - Success: retained flows pass 1.2.1 without constructing any UI adapter.
  - Requires 1.1.2 and 1.2.1.

- [ ] 2.1.2. Make `webmcp` an explicit opt-in Cargo feature.
  - Gate dependencies, commands, activation, listeners, and composition.
  - Remove any WebMCP dependence on TUI handles; retain explicit runtime
    authentication and listener authorization in the enabled build.
  - Success: default builds cannot start the bridge; explicit feature builds
    pass bridge tests; ordinary MCP and provider WebSockets remain functional.
  - Requires 1.1.2; coordinate shared-boundary changes with 2.1.1.

### 2.2. Delete the TUI and its exclusive dependency closure

- [ ] 2.2.1. Delete the UI crate and all remaining TUI implementations.
  - Remove screens, overlays, raw-mode input, rendering, themes, TUI-only
    clipboard/notifications, fixtures, examples, and benchmarks.
  - Remove the `tui` feature, public re-exports, and the LLM Ratatui edge.
  - Success: no dormant feature, vendored copy, no-op renderer, or renamed TUI
    remains; retained shell/PTY functionality passes its existing tests.
  - Requires 2.1.1 and the relevant bridge separation in 2.1.2.

- [ ] 2.2.2. Reconcile workspace, packaging, CI, and dependency metadata.
  - Remove UI membership/default-members, patches, build scripts, install
    assumptions, platform-only presentation dependencies, and obsolete notices.
  - Keep default product builds from selecting the optional WebMCP member.
  - Success: locked default, minimal, WebMCP, and supported all-features builds
    resolve the intended graphs across supported platforms.
  - Requires 2.1.2 and 2.2.1.

## 3. Prove and publish the new product boundary

Idea: enforceable graph/source checks and honest measurements prevent the UI
from returning through a convenience import or a release script.

### 3.1. Ratchet and qualify the build

- [ ] 3.1.1. Add source and resolved-dependency regression checks.
  - Check the shipped root closure and explicit feature combinations, not just
    lockfile package names or `--no-default-features`.
  - Success: a seeded UI import or default WebMCP edge fails CI; historical
    documentation and legitimate non-UI HTTP/image/PTY consumers remain valid.
  - Requires 2.2.2.

- [ ] 3.1.2. Run functional equivalence and resource comparisons.
  - Repeat 1.2.1 and 1.2.2 under identical workload and build conditions.
  - Success: preserved contracts pass and a report publishes clean/warm build
    cost, compiler/linker peak memory, artifact size, ACP startup, and PSS.
    No numerical improvement is claimed from a changed allocator or worker pool.
  - Requires 3.1.1.

### 3.2. Complete migration and remove stale guidance

- [ ] 3.2.1. Publish release migration guidance and refreshed references.
  - Update help, completions, generated schemas, README, active user/developer
    guides, crate instructions, and CI references to removed UI checks.
  - Document explicit WebMCP installation and feature-disabled errors.
  - Success: active documentation describes ACP/CLI only; local links pass and
    no currently advertised command requires the deleted interface.
  - Requires 1.1.2 and 3.1.2.

- [ ] 3.2.2. Record completion and residual non-UI work.
  - Reconcile affected programme issues with their owners; retain useful
    application lifecycle/security work and avoid automatic bulk closure.
  - Success: evidence demonstrates both complete TUI deletion and default
    WebMCP exclusion; remaining work does not reintroduce either dependency.
  - Requires 3.2.1.
