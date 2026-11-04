# diapason-* Crate Audit Report

**Date**: 2026-06-14 (updated 2026-06-14) **Scope**: All diapason-* workspace
crates **Goal**: Identify mergeable crates, redundant code, and test duplicates

> **Note**: Several merges from this audit have been completed. See "Completed
> Merges" section below.

______________________________________________________________________

## Workspace Overview

| Crate                       | Source Files | Inline Tests | Purpose                                                                | Deps on other diapason-*                        |
| --------------------------- | ------------ | ------------ | ---------------------------------------------------------------------- | ----------------------------------------------- |
| diapason-commons            | 47           | 22           | Shared primitives (ANSI, diff, errors, paths, tokens, walk, telemetry) | none                                            |
| diapason-core               | 828          | 466          | Main agent runtime (LLM, tools, skills, agents, MCP)                   | 12 crates                                       |
| diapason-config             | 120          | 32           | Config loading, schema, models, defaults, hooks                        | commons, auth                                   |
| diapason-ui                 | 184          | 61           | Design system, theme registry, TUI framework                           | commons, config, terminal-detection, vim        |
| diapason-acp                | 32           | 13           | Agent Communication Protocol (Zed adapter)                             | core, config                                    |
| diapason-auth               | 10           | 8            | OAuth, PKCE, credential storage                                        | none (leaf)                                     |
| diapason-bash-runner        | 9            | 5            | Shell execution, process groups                                        | commons, exec-events                            |
| diapason-exec-events        | 3            | 3            | Structured execution telemetry event schemas                           | none (leaf)                                     |
| diapason-file-search        | 2            | 1            | Fast fuzzy file search (nucleo)                                        | commons                                         |
| diapason-indexer            | 1            | 1            | Workspace file indexer with markdown persistence                       | commons                                         |
| diapason-macros             | 1            | 0            | `#[derive(StringNewtype)]` proc macro                                  | none (leaf)                                     |
| diapason-markdown-store     | 1            | 1            | Markdown-backed storage (KV, project, cache)                           | none (leaf)                                     |
| diapason-process-hardening  | 1            | 0            | Pre-main process security hardening                                    | none (leaf)                                     |
| diapason-terminal-detection | 1            | 1            | Terminal emulator detection                                            | none (leaf)                                     |
| diapason-utility-tool-specs | 5            | 4            | JSON schemas for tool parameter definitions                            | none (leaf)                                     |
| diapason-vim                | 4            | 1            | Vim-style prompt editing engine                                        | none (leaf)                                     |
| diapason-tool-types         | 6            | 0            | Shared tool runtime types (breaks circular deps)                       | none (leaf)                                     |
| diapason-llm                | ~100         | 0            | LLM provider abstraction, client implementations, streaming            | commons, config, tool-types, utility-tool-specs |
| diapason-skills             | 22           | 0            | Skill types, discovery, loading, and validation                        | commons, config                                 |
| diapason-safety             | 15           | 31           | Command safety detection, execution policies, sandboxing               | commons                                         |
| diapason-pods               | 6            | 0            | GPU pod management                                                     | commons                                         |
| diapason-a2a                | 10           | 6            | Agent2Agent protocol client and server                                 | none (leaf)                                     |
| diapason-mcp                | 15           | 31           | MCP client, connection pooling, tool discovery                         | config, commons, utility-tool-specs             |

**Dependency graph** (leaf crates at bottom):

```
                          diapason (binary)
                        /    |    \      \
                   diapason-acp  |  diapason-ui  diapason-process-hardening
                     /    \    |    /    \  \
              diapason-core  diapason-config | diapason-vim  diapason-terminal-detection
              / | | | \ \  \    /   \      |
    commons auth bash exec file idx md-store
         macros    events search         utility-tool-specs
              \      |      /
               diapason-safety (command_safety + exec_policy + sandboxing)
               diapason-llm (LLM providers, client abstraction, streaming)
               diapason-skills (skill types, discovery, validation)
                    diapason-pods (GPU management)
                    diapason-a2a (Agent2Agent protocol)
                    diapason-tool-types (shared types, breaks circular deps)
                    diapason-mcp (MCP client, connection pooling, tool discovery)
```

______________________________________________________________________

## MERGE RECOMMENDATIONS

### MERGE 1: diapason-indexer + diapason-file-search -> diapason-search (HIGH impact)

**Rationale**: Both crates provide file discovery/indexing capabilities with
overlapping domain concepts.

**Evidence**:

- Both define a `pub struct FileIndex` with different fields but the same name
- diapason-file-search provides fuzzy matching (nucleo); diapason-indexer
  provides regex-based search + markdown persistence
- diapason-core integrates both through `tools/file_search_bridge.rs` and
  `tools/file_search_rpc.rs`
- diapason-indexer already uses `diapason-commons::walk` for traversal, same as
  file-search would
- Combined crate would be ~500 lines, still small enough to be focused

**Merged crate responsibilities**:

- File indexing (hash, metadata, content) from indexer
- Fuzzy search (nucleo) from file-search
- Regex/grep search from indexer
- Markdown-backed index persistence from indexer
- `FileIndexCache` from file-search

**Risk**: Medium. The two `FileIndex` structs need unification. The indexer's
`MarkdownIndexStorage` depends on `fs2` for locking which file-search doesn't
need. Feature-gate the persistence behind an optional dep.

**Savings**: 1 fewer crate in workspace, 1 fewer bridge layer in diapason-core,
unified `FileIndex` type.

______________________________________________________________________

### MERGE 2: diapason-exec-events into diapason-commons (MEDIUM impact)

**Rationale**: diapason-exec-events is a tiny leaf crate (3 files, 898 lines)
that only defines event schema types. diapason-commons already contains
foundational types (LLM types, telemetry traits, error categories).

**Evidence**:

- diapason-exec-events depends on nothing -- pure data types
- diapason-commons already defines `TelemetrySink<Event>` trait and `Usage`
  struct
- Both define a `Usage` struct (diapason-commons uses u32, exec-events uses u64)
  -- these should be unified
- diapason-bash-runner optionally depends on exec-events; it already depends on
  commons
- Moving exec-events into `diapason-commons::exec_events` module would eliminate
  a crate and resolve the dual-Usage conflict

**Merged location**: `crates/common/diapason-commons/src/exec_events/` module

**Risk**: Low. Pure schema types with no complex logic. The u32->u64 Usage
migration needs careful type width audit.

**Savings**: 1 fewer crate, unified `Usage` type, cleaner dependency graph.

______________________________________________________________________

### MERGE 3: diapason-markdown-store into diapason-commons (MEDIUM impact)

**Rationale**: diapason-markdown-store is a single-file crate (653 lines)
providing markdown-backed persistence. diapason-commons already provides file
system primitives (`fs`, `paths`, `walk`).

**Evidence**:

- Only 1 source file, no inter-crate deps
- Provides `MarkdownStorage`, `SimpleKVStorage`, `ProjectStorage`, `SimpleCache`
- diapason-indexer implements its own `MarkdownIndexStorage` instead of reusing
  this -- the consolidation in MERGE 1 would bring this into the combined
  search crate anyway
- `SimpleCache` (feature-gated) overlaps conceptually with diapason-core's
  `UnifiedCache`

**Merged location**: `crates/common/diapason-commons/src/markdown_store/` module

**Risk**: Low. Self-contained, no complex dependencies.

**Savings**: 1 fewer crate, consistent storage primitives.

______________________________________________________________________

### MERGE 4: diapason-process-hardening into diapason (binary crate) (LOW impact)

**Rationale**: diapason-process-hardening is a single-file crate (421 lines)
with a single `#[ctor]` function that runs before `main`. It's only consumed by
the top-level binary crate.

**Evidence**:

- Only used by `diapason` binary (the top-level crate)
- No other diapason-* crate depends on it
- It's a `#[ctor]` that runs pre-main -- there's no reason it needs to be a
  separate library crate
- Moving the code into `src/main.rs` or a `src/hardening.rs` module in the
  binary crate is simpler

**Risk**: Very low. The `#[ctor]` attribute works identically in binary crate
modules.

**Savings**: 1 fewer crate, simpler build graph.

______________________________________________________________________

### NO MERGE: Crates that should stay separate

| Crate                       | Why keep separate                                                       |
| --------------------------- | ----------------------------------------------------------------------- |
| diapason-commons            | Foundational leaf crate, everything depends on it. Keep focused.        |
| diapason-core               | The hub. 828 files, 466 test modules. Too large to merge anything into. |
| diapason-config             | Large (120 files), clean separation of config from runtime.             |
| diapason-ui                 | Large (184 files), complex TUI framework. Clean boundary.               |
| diapason-acp                | Protocol-specific, depends on core. Clean adapter pattern.              |
| diapason-auth               | OAuth flows are self-contained and security-sensitive.                  |
| diapason-bash-runner        | Shell execution is a distinct concern with its own safety model.        |
| diapason-macros             | Proc-macro crates must be separate by Rust rules.                       |
| diapason-terminal-detection | Clean, focused, no overlap.                                             |
| diapason-utility-tool-specs | Passive JSON schemas, zero deps, no overlap.                            |
| diapason-vim                | Self-contained editor engine, clean boundary.                           |

______________________________________________________________________

## RE-EXPORT CLEANUP (in diapason-core)

diapason-core re-exports types from other crates through 3+ import paths. These
should be consolidated.

### Token estimation functions (3 paths -> 1)

Current state:

- `diapason_commons::tokens::{estimate_tokens, truncate_to_tokens}` -- canonical
- `diapason_core::llm::utils::{estimate_token_count, truncate_to_token_limit}`
  -- re-export with alias
- `diapason_core::utils::tokens::*` -- wildcard re-export

**Action**: Keep `diapason_commons::tokens` as canonical. Remove
`diapason_core::utils::tokens` re-export. Rename `diapason_core::llm::utils`
re-exports to use original names.

### HTTP client (3 paths -> 1)

Current state:

- `diapason_commons::http::*` -- canonical
- `diapason_core::http_client::*` -- re-export
- `diapason_core::utils::http_client::*` -- re-export

**Action**: Keep `diapason_commons::http` as canonical. Remove both re-export
files in diapason-core.

### Error types (3 paths -> 2)

Current state:

- `diapason_commons::errors::*` and `diapason_commons::error_category::*` --
  canonical
- `diapason_core::error::{DiapasonError, ErrorCode}` -- diapason-core-specific
  types
- `diapason_core::utils::error_messages::*` -- re-export of commons

**Action**: Remove `diapason_core::utils::error_messages` re-export. Keep
diapason-core's own error types separate from commons.

### Config types (2 paths -> 1)

Current state:

- `diapason_config::*` -- canonical
- `diapason_core::config::*` -- 11+ re-export files mirroring diapason-config

**Action**: This re-export chain exists for backward compatibility. It's
intentional and low-risk, but should be documented as deprecated import paths.
Add `#[deprecated]` attributes to guide consumers toward `diapason_config::*`.

______________________________________________________________________

## TEST DUPLICATES TO ELIMINATE

### HIGH priority

1. **Delete `diapason-core/tests/mcp_basic_test.rs`** (160 lines)
   - Every test is a duplicate of tests in `mcp_integration_test.rs`
   - `test_mcp_config_defaults`, `test_mcp_client_creation`,
     `test_mcp_ui_modes`, `test_provider_environment_variables`,
     `test_mcp_client_initialization` are all near-identical

2. **Remove duplicated tests from `diapason-core/tests/mcp_integration_e2e.rs`**
   - `test_mcp_ui_modes` -- duplicates `mcp_basic_test.rs` line 79 (which
     itself duplicates `mcp_integration_test.rs`)
   - `test_provider_environment_variables` -- duplicates `mcp_basic_test.rs`
     line 103
   - `test_multiple_providers_config` -- duplicates `mcp_integration_test.rs`
     line 165
   - `test_mcp_client_status` -- duplicates `mcp_integration_test.rs` line 294
   - E2E file should focus only on real-server lifecycle tests

### MEDIUM priority

1. **Merge `diapason-core/tests/unicode_handling_test.rs` into
   `pty_unicode_test.rs`**
   - Both test the same unicode edge cases (emojis, CJK, split sequences)
   - `unicode_handling_test.rs` never instantiates a PTY -- it's pure UTF-8
     exercises

2. **Consolidate test helpers into `diapason-core/tests/support/`**
   - `is_python_available()` -- duplicated in `mcp_integration_e2e.rs:37` and
     `mcp_startup_timeout_test.rs:183`
   - `mock_mcp_server_path()` -- duplicated in `mcp_integration_e2e.rs:44` and
     `mcp_startup_timeout_test.rs:190`
   - `setup_registry()` -- duplicated in `apply_patch_comprehensive.rs:6` and
     `apply_patch_semantic.rs:9`

3. **Create shared `FileOpsTool` test helper**
   - The 2-line `GrepSearchManager::new` + `FileOpsTool::new` pattern appears
     15+ times across test files
   - Move to `diapason-core/tests/support/mod.rs`

### LOW priority

1. **Rename PTY test files for clarity**
   - `pty_test.rs` -> `pty_registry_integration_tests.rs` (tests through
     ToolRegistry)
   - `pty_tests.rs` -> `pty_manager_unit_tests.rs` (tests PtyManager directly)

2. **Consolidate optimization tests**
   - `real_optimization_integration_test.rs` and
     `real_execute_tool_ref_optimization_test.rs` overlap significantly

______________________________________________________________________

## IMPLEMENTATION ORDER

If proceeding with merges, the recommended order is:

1. **MERGE 4** (process-hardening into binary) -- zero risk, smallest change
2. **MERGE 2** (exec-events into commons) -- resolve Usage type conflict
3. **MERGE 3** (markdown-store into commons) -- clean up storage primitives
4. **MERGE 1** (indexer + file-search into search) -- largest change, depends
   on 2+3 being done first
5. **Test cleanup** -- can be done in parallel with merges
6. **Re-export cleanup** -- do last, after all merges are stable

Each merge should be a separate PR with:

- `cargo check --workspace` passing
- `cargo test --workspace` passing
- Updated AGENTS.md for affected crates
- Deprecation annotations on old import paths

______________________________________________________________________

## COMPLETED MERGES (2026-06-14)

The following merges from this audit have been implemented:

| Merge   | From                                           | To                     | Status                    |
| ------- | ---------------------------------------------- | ---------------------- | ------------------------- |
| MERGE 4 | diapason-process-hardening                     | diapason (root binary) | Completed                 |
| MERGE 5 | diapason-terminal-detection                    | diapason-commons       | Completed                 |
| MERGE 6 | diapason-vim                                   | diapason-ui            | Completed                 |
| MERGE 7 | diapason-pods                                  | diapason-core          | Completed                 |
| MERGE 8 | diapason-tool-types                            | diapason-commons       | Completed                 |
| MERGE 9 | diapason-file-search + diapason-markdown-store | diapason-indexer       | Completed (prior session) |

### What changed

- **diapason-process-hardening**: Moved to `src/process_hardening.rs` in root
  binary crate
- **diapason-terminal-detection**: Moved to
  `crates/common/diapason-commons/src/terminal_detection.rs`
- **diapason-vim**: Moved to `crates/codegen/diapason-ui/src/vim/` module
- **diapason-pods**: Moved to `crates/codegen/diapason-core/src/pods/` module
- **diapason-tool-types**: Merged into `diapason-commons` (tool_types,
  model_family modules)
- **diapason-file-search + diapason-markdown-store**: Merged into
  diapason-indexer (prior session)

### Benefits realized

- 6 fewer crates in workspace (from 23 to 17 member crates)
- Reduced cross-crate dependency complexity
- Simpler import paths for shared types
- Fewer Cargo.toml files to maintain
