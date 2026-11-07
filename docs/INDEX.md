# Documentation Index

Repository-wide entrypoint for Diapason documentation.

Last reviewed: 2026-11-03

## Start Here

- [Community Wiki](https://github.com/leynos/vtcode/wiki) - Getting started,
  configuration, providers, local models, skills, MCP, automation, security,
  and FAQ.
- [Documentation Hub](README.md) - Main user/developer overview.
- [User Guide](users-guide.md) - Links to canonical user setup and usage docs.
- [Developer Guide](developers-guide.md) - Links to canonical architecture and
  contributor docs.
- [0.157.0 release notes](releases/0.157.0.md) - Migration and ACP behavior
  notes.
- [Harness Index](harness/INDEX.md) - Agent operating model, quality scoring,
  and debt tracking.
- [Zen Alignment](harness/ZEN_ALIGNMENT.md) - Full all-19 principle mapping and
  rollout.
- [Architecture](ARCHITECTURE.md) - System structure and crate boundaries.
- [Contributing](CONTRIBUTING.md) - Contribution workflow and standards.

## Core Domains

- [Configuration Precedence](config/CONFIGURATION_PRECEDENCE.md) - Runtime
  config loading order.
- [User Data Directories](guides/user-data-directories.md) - XDG/native paths,
  environment overrides, permissions, and legacy migration.
- [Zed ACP Guide](guides/zed-acp.md) - Zed Agent Client Protocol setup and
  session behaviour.
- [ACP Runtime Developer Guide](development/acp-runtime.md) - Runtime
  boundaries, persistence, provider policy, tools, and protocol updates.
- [Instruction Discovery Boundary](development/instruction-discovery.md) -
  Home-directory injection, test isolation, and include/exclude error contracts.
- [Stale exec sessions](users-guide.md#stale-write_stdin-sessions) -
  Process-local exec-session identifiers and the non-retryable stale-session
  error.
- [Config Field Reference](config/CONFIG_FIELD_REFERENCE.md) - Field-level
  schema reference.
- [Tool Specifications](tools/TOOL_SPECS.md) - Canonical tool surface reference.
- [Patch Safety](guides/patch-safety.md) - File-version preconditions and the
  repeated no-op guard for `apply_patch`.
- [AI Tool Surface Migration](development/ai-tool-surface-migration.md) -
  Breaking-change notes for the Codex-style default tools.
- [Web Search Tool](tools/web_search.md) - Web search configuration and guard
  rails.
- [Defuddle Fetch](tools/defuddle_fetch.md) - Markdown extraction fallback for
  complex pages.
- [Scheduled Tasks](user-guide/scheduled-tasks.md) - Reminder and durable
  scheduler flows.
- [WebMCP Browser Bridge User Guide](user-guide/webmcp.md) - Connect a browser
  editor to an active Diapason session or a bounded standalone workspace bridge.
- [Planning Workflow](guides/planning-workflow.md) - `/plan`, review gate, plan
  handoff to build/auto agents.
- [Permissions Guide](guides/permissions.md) - Granular agent permissions and
  rule grammar.
- [Provider Guides](providers/PROVIDER_GUIDES.md) - LLM provider setup and
  behavior.
- [Security Model](security/SECURITY_MODEL.md) - Security architecture.
- [Process Hardening](development/PROCESS_HARDENING.md) - Runtime hardening
  controls.
- [MCP Integration Guide](mcp/MCP_INTEGRATION_GUIDE.md) - MCP integration
  onboarding.
- [Agent Plugins](guides/agent-plugins.md) - Portable skill + MCP packages,
  `diapason plugins` CLI.
- [Testing Guide](development/testing.md) - Test strategy and commands.

## Engineering References

- [Development Setup](development/DEVELOPMENT_SETUP.md) - Canonical contributor
  setup and local quality loop.
- [Documentation style guide](documentation-style-guide.md) - Authoring
  standards, including RFC and ADR filenames.
- [C++ Core Guidelines Adoption](development/CPP_CORE_GUIDELINES_ADOPTION.md) -
  Rules for C/C++ code paths and cross-language safety intent.
- [Extension Boundaries](development/EXTENSION_BOUNDARIES.md) - When to use
  internal Rust traits vs external protocol or manifest seams.
- [Language Support Matrix](protocols/LANGUAGE_SUPPORT.md) - Tree-sitter and
  language support status.
- [Signal Handling](signal_handling.md) - Ctrl+C / SIGINT priority guarantees
  and emergency exit.
- [Indexer Notes](modules/diapason_indexer.md) - Indexer behavior and usage.
- [Development Guide](development/README.md) - Local dev workflows.
- [Roadmap](project/ROADMAP.md) - Planned work.
- [Loop Engineering](loop-engineering.md) - Worktree isolation, propose/verify
  sub-agents, loop state, cost guardrails.

## Proposed Diapason observability

- [Observability RFC](rfcs/rfc-0002-diapason-observability.md) - Requirements,
  prior-art parity and trade-offs; proposed, not implemented.
- [Observability technical design](design/diapason-observability-design.md) -
  Rust ownership, telemetry contracts, privacy and operational guarantees.
- [Observability GIST roadmap](project/diapason-observability-roadmap.md) -
  Delivery slices and measurable acceptance criteria.

## Module Documentation

- [diapason-ui](modules/diapason_ui.md) - UI framework, design system, theme
  registry.
- [diapason-llm](modules/diapason_llm.md) - LLM provider abstraction and
  streaming.
- [diapason-skills](modules/diapason_skills.md) - Skill discovery, loading, and
  validation.
- [diapason-safety](modules/diapason_safety.md) - Command safety, execution
  policies, sandboxing.
- [diapason-a2a](modules/diapason_a2a.md) - Agent2Agent protocol support.
- [diapason-mcp](modules/diapason_mcp.md) - Model Context Protocol integration.
- [diapason-bash-runner](modules/diapason_bash_runner.md) - Shell execution
  sandbox.
- [diapason-config](modules/diapason_config_migration.md) - Configuration
  loading and schema.
- [diapason-commons](modules/diapason_commons_reference.md) - Shared utilities.
- [diapason-exec-events](modules/diapason_exec_events.md) - ThreadEvent contract
  and ATIF export.
- [diapason-indexer](modules/diapason_indexer.md) - Code indexing and search.
- [diapason-agent-plugins](modules/diapason_agent_plugins.md) - Agent Plugins
  manifest parsing, validation, and discovery.

## Historical and Archive Paths

- [Documentation archive](archive/README.md) - Historical reports and upstream
  records, with their original subdirectories preserved.

When adding implementation summaries or one-off reports, prefer a domain folder
or an archive path instead of placing files at `docs/*.md`.
