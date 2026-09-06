<div align="center">

<picture>
  <img src="./resources/logo/vt_code_adaptive.svg" alt="VT Code" width="300" />
</picture>

**Secure, open, universal terminal coding agent in Rust.**

[![Agent Skills](https://img.shields.io/badge/Agent_Skills-BFB38F?style=flat-square)](https://agentskills.io/)
[![Agent Client Protocol](https://img.shields.io/badge/Agent_Client_Protocol-383B73?style=flat-square&logo=zedindustries&logoColor=white)](./docs/guides/zed-acp.md)
[![Model Context Protocol](https://img.shields.io/badge/Model_Context_Protocol-A63333?style=flat-square&logo=modelcontextprotocol&logoColor=white)](./docs/guides/mcp-integration.md)
[![Agent Plugins](https://img.shields.io/badge/Agent_Plugins-5865F2?style=flat-square)](./docs/guides/agent-plugins.md)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/vinhnx/VTCode)

</div>

> [!TIP]
> New here? Start with [Installation](./docs/installation/README.md), then
> [Getting Started](./docs/user-guide/getting-started.md).

<details>
<summary><strong>Contents</strong></summary>

- [Overview](#overview)
- [Why VT Code](#why-vt-code)
- [Quick start](#quick-start)
  - [1. Install](#1-install)
  - [2. Configure](#2-configure)
  - [3. Run](#3-run)
  - [WebMCP browser bridge (opt-in)](#webmcp-browser-bridge-opt-in)
- [Documentation](#documentation)
- [Providers](#providers)
- [Development](#development)
- [Contributing](#contributing)
  - [Ways to contribute](#ways-to-contribute)
  - [Getting started](#getting-started)
  - [Contributors](#contributors)
- [Support](#support)
  - [Sponsorship](#sponsorship)
- [License](#license)

</details>

## Overview

<div align="center">

<img src="./resources/gif/vtcode.gif" alt="VT Code demo" width="60%" />
<br />
<em>Secure, open, universal.</em>

</div>

VT Code is an open-source Rust terminal coding agent for interactive and
long-running autonomous work. It is a **harness, not just an LLM wrapper**:
the model provides reasoning, while the runtime provides the tools, context,
sandbox, state, evaluation, and verification needed to turn that reasoning into
safe, reviewable progress.

A responsive TUI, multi-provider LLM support, durable sessions, open protocols,
and extensible Skills take you from question to reviewed change without leaving
the terminal.

> [!NOTE]
> **Status:** Active development. Local inference and some automation flows are
> experimental and may change between releases.

> [!TIP]
> **Behind the build:** [Building VT Code, a year in](https://huggingface.co/blog/vinhnx90/building-vtcode-a-year-in):
> harness design, evals, security, and lessons from a year of building.
>
> **Video companions:** [Podcast](https://www.youtube.com/watch?v=XLoswcd5rH0) ·
> [Video](https://www.youtube.com/watch?v=PvL_kPjgU6o).

| Podcast companion                                                                                                                                                            | Video companion                                                                                                                                                            |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| <a href="https://www.youtube.com/watch?v=XLoswcd5rH0"><img src="https://img.youtube.com/vi/XLoswcd5rH0/maxresdefault.jpg" alt="VT Code podcast companion" width="400" /></a> | <a href="https://www.youtube.com/watch?v=PvL_kPjgU6o"><img src="https://img.youtube.com/vi/PvL_kPjgU6o/maxresdefault.jpg" alt="VT Code video companion" width="400" /></a> |
| [Watch the podcast](https://www.youtube.com/watch?v=XLoswcd5rH0)                                                                                                             | [Watch the video](https://www.youtube.com/watch?v=PvL_kPjgU6o)                                                                                                             |

## Why VT Code

| Pillar                     | What it means                                                                                                                                 |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| **Harness, not a wrapper** | The model reasons; the harness composes tools, context, sandbox, state, and evaluations to enforce progress.                                  |
| **Safety-first execution** | Sandboxed shell, command policies, workspace approvals, and fail-closed defenses for injection, path/symlink escape, and environment leakage. |
| **Long-run reliability**   | Durable session memory, task tracking, spooled output, checkpoints, automatic compaction, resumable handoffs, and verification before "done". |
| **Observable by design**   | A canonical `ThreadEvent` runtime contract supports replay, archives, checkpoints, memory views, and trajectory export.                       |
| **Protocol-native**        | MCP, Skills, Agent Plugins, ACP (Zed), A2A, WebMCP, Open Responses, and ATIF extend the system without core forks.                            |
| **Controlled autonomy**    | Planning, human approval, isolated worktrees, propose/verify sub-agents, full automation, and cost guardrails scale autonomy safely.          |
| **Runs anywhere**          | 30 providers plus local Ollama, LM Studio, and llama.cpp.                                                                                     |

## Quick start

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/vinhnx/vtcode/main/scripts/install.sh | bash
# or: brew install vinhnx/tap/vtcode | cargo install vtcode
```

### 2. Configure

```bash
cd path/to/your/project
vtcode init                    # scaffolds config + AGENTS.md; review before committing
export OPENAI_API_KEY="sk-..." # or `vtcode login` for OAuth providers
```

### 3. Run

```bash
vtcode                         # interactive TUI
vtcode ask "explain Rc vs Arc" # one-shot question
vtcode exec "refactor main.rs" # headless task
vtcode review                  # review uncommitted changes
```

See [Installation](./docs/installation/README.md) and
[Getting Started](./docs/user-guide/getting-started.md).

> [!CAUTION]
> Never commit API keys or put them in `vtcode.toml`.

### WebMCP browser bridge (opt-in)

> [!NOTE]
> Pair a running session from the TUI or serve a bounded workspace for
> authenticated browser editing.

```bash
# Inside the TUI:
/webmcp pair <origin>

# Or serve a bounded workspace:
vtcode webmcp serve --origin <origin> --allowed-root <dir>
```

| Host       | Link                                                      |
| ---------- | --------------------------------------------------------- |
| Hosted app | <https://vtcode.vinhnx.chatgpt.site/>                     |
| Fallback   | <https://vinhnx.github.io/VTCode/>                        |
| User guide | [WebMCP user guide](./docs/user-guide/webmcp.md)          |
| Deployment | [WebMCP deployment reference](./docs/reference/webmcp.md) |

## Documentation

| Area    | Guides                                                                                                                                                                                                                                                                                                                                                                                            |
| ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Start   | [Installation](./docs/installation/README.md) · [Getting started](./docs/user-guide/getting-started.md) · [Wiki](https://github.com/vinhnx/VTCode/wiki) · [Blog: Building VT Code, a year in](https://huggingface.co/blog/vinhnx90/building-vtcode-a-year-in) · [Podcast companion](https://www.youtube.com/watch?v=XLoswcd5rH0) · [Video companion](https://www.youtube.com/watch?v=PvL_kPjgU6o) |
| Use     | [TUI](./docs/user-guide/interactive-mode.md) · [CLI](./docs/user-guide/commands.md) · [WebMCP](./docs/user-guide/webmcp.md) · [Automation](./docs/guides/full-automation.md) · [Planning](./docs/guides/planning-workflow.md) · [Configuration](./docs/config/CONFIG_FIELD_REFERENCE.md)                                                                                                          |
| Extend  | [Skills](./docs/skills/SKILLS_GUIDE.md) · [Plugins](./docs/guides/agent-plugins.md) · [MCP](./docs/guides/mcp-integration.md) · [Editors](./docs/guides/zed-acp.md)                                                                                                                                                                                                                               |
| Operate | [Safety](./docs/security/SECURITY_MODEL.md) · [Protocols](./docs/protocols/OPEN_RESPONSES.md) · [Loop engineering](./docs/project/PLAN-loop-engineering.md) · [Architecture](./docs/ARCHITECTURE.md)                                                                                                                                                                                              |

## Providers

30 built-in providers, custom OpenAI-compatible endpoints, and local backends.
[Provider Guides](./docs/providers/PROVIDER_GUIDES.md) is the source of truth
for credentials and model defaults.

Custom providers support opt-in pricing in USD per million tokens. ACP emits
`costUSD` only when both input and output rates are configured; see
[custom-provider pricing](./docs/config/config.md).

```bash
vtcode models list
vtcode models config
```

> [!TIP]
> Restrict providers per workspace with `providers_whitelist` in `vtcode.toml`.
> Local inference (experimental) via Ollama, LM Studio, and llama.cpp is managed
> with `/local` in the TUI - see [Local Models](./docs/guides/local-models.md).

## Development

```mermaid
graph LR
    types --> config --> core --> tools --> agent --> TUI
```

1. Clone and run the fast gate:

```bash
git clone https://github.com/vinhnx/vtcode.git
cd vtcode
./scripts/run-debug.sh
./scripts/check-dev.sh   # fast gate: clippy, fmt, check
cargo nextest run        # tests (never `cargo test`)
```

Rust stable, edition 2024. ~30 crates layered as
`types → config → core → tools → agent → TUI`, with `ThreadEvent` as the
authoritative runtime contract.

See [Development setup](./docs/development/DEVELOPMENT_SETUP.md) and
[Testing](./docs/development/testing.md).

## Contributing

VT Code grows with its community. Bug fixes, docs, ideas, testing, and
reviews are all welcome.

### Ways to contribute

- **Security advisories**: Please report vulnerabilities privately first: [Security Policy](https://github.com/vinhnx/VTCode/security/policy).
- **Bug fixes and patches**: Small or large, every fix counts.
- **Documentation**: Guides, examples, and corrections help everyone.
- **Features and ideas**: Open an issue or start a discussion.
- **Code reviews and testing**: Trying things out and reporting breakage keeps the project healthy.

### Getting started

- Browse [good first issues](https://github.com/vinhnx/vtcode/issues?q=is%3Aopen+is%3Aissue+label%3A%22good+first+issue%22)
- Read [CONTRIBUTING.md](./docs/CONTRIBUTING.md) for humans
- Check [AGENTS.md](./AGENTS.md) for AI agents

> [!NOTE]
> Small, focused PRs merge fastest. If you get stuck, open an issue for help.

### Contributors

Thank you to everyone who shaped VT Code.

<details>
<summary><strong>Show all contributors</strong></summary>

<div align="center">
  <a href="https://github.com/kernitus"><img src="https://avatars.githubusercontent.com/u/2789734?s=60" width="40" height="40" alt="@kernitus" title="@kernitus Main Contributor (52 commits)" style="border-radius: 50%; border: 2px solid #FFD700;" /></a>&nbsp;
  <a href="https://github.com/7jrxt42BxFZo4iAnN4CX"><img src="https://avatars.githubusercontent.com/u/72938937?s=60" width="40" height="40" alt="@7jrxt42BxFZo4iAnN4CX" title="@7jrxt42BxFZo4iAnN4CX Core contributor (40 commits) - subagents, hooks, config & TUI fixes (#737, #738, #740-#742+)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/oiwn"><img src="https://avatars.githubusercontent.com/u/398035?s=60" width="40" height="40" alt="@oiwn" title="@oiwn Core contributor (6 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/Sachin-Bhat"><img src="https://avatars.githubusercontent.com/u/25080916?s=60" width="40" height="40" alt="@Sachin-Bhat" title="@Sachin-Bhat Core contributor (3 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/chenrui333"><img src="https://avatars.githubusercontent.com/u/1580956?s=60" width="40" height="40" alt="@chenrui333" title="@chenrui333 Core contributor (3 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/gzsombor"><img src="https://avatars.githubusercontent.com/u/66230?s=60" width="40" height="40" alt="@gzsombor" title="@gzsombor Core contributor (2 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/leonj1"><img src="https://avatars.githubusercontent.com/u/5171829?s=60" width="40" height="40" alt="@leonj1" title="@leonj1 Core contributor (2 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/netbrah"><img src="https://avatars.githubusercontent.com/u/162479981?s=60" width="40" height="40" alt="@netbrah" title="@netbrah Core contributor (2 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/xcrong"><img src="https://avatars.githubusercontent.com/u/46434477?s=60" width="40" height="40" alt="@xcrong" title="@xcrong Core contributor (2 commits)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/lucaszhu-hue"><img src="https://avatars.githubusercontent.com/u/278269343?s=60" width="40" height="40" alt="@lucaszhu-hue" title="@lucaszhu-hue Core contributor (2 commits) - Atlas Cloud (#648, #662)" style="border-radius: 50%; border: 2px solid #50C878;" /></a>&nbsp;
  <a href="https://github.com/raphamorim"><img src="https://avatars.githubusercontent.com/u/3630346?s=60" width="40" height="40" alt="@raphamorim" title="@raphamorim PR #708, rio-vt migration (1 commit)" style="border-radius: 50%; border: 2px solid #4A90D9;" /></a>&nbsp;
  <a href="https://github.com/nnfrog"><img src="https://avatars.githubusercontent.com/u/142202920?s=60" width="40" height="40" alt="@nnfrog" title="@nnfrog GHSA-r249-hpfx-x2w7 (security advisory)" style="border-radius: 50%; border: 2px solid #FF6B6B;" /></a>&nbsp;
  <a href="https://github.com/glmgbj233"><img src="https://avatars.githubusercontent.com/u/115564047?s=60" width="40" height="40" alt="@glmgbj233" title="@glmgbj233 GHSA-wqgw-crr5-cr2p (security advisory)" style="border-radius: 50%; border: 2px solid #FF6B6B;" /></a>&nbsp;
  <a href="https://github.com/EvoLinkAI"><img src="https://avatars.githubusercontent.com/u/253253881?s=60" width="40" height="40" alt="@EvoLinkAI" title="@EvoLinkAI Contributor (1 commit) - Evolink provider (#664)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/diegosouzapw"><img src="https://avatars.githubusercontent.com/u/8016841?s=60" width="40" height="40" alt="@diegosouzapw" title="@diegosouzapw Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/ForrestThump"><img src="https://avatars.githubusercontent.com/u/44280834?s=60" width="40" height="40" alt="@ForrestThump" title="@ForrestThump Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/morler"><img src="https://avatars.githubusercontent.com/u/478444?s=60" width="40" height="40" alt="@morler" title="@morler Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/poelzi"><img src="https://avatars.githubusercontent.com/u/66107?s=60" width="40" height="40" alt="@poelzi" title="@poelzi Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/RobertBorg"><img src="https://avatars.githubusercontent.com/u/1288566?s=60" width="40" height="40" alt="@RobertBorg" title="@RobertBorg Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/Sanjays2402"><img src="https://avatars.githubusercontent.com/u/51058514?s=60" width="40" height="40" alt="@Sanjays2402" title="@Sanjays2402 Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/TuanLe-bk18"><img src="https://avatars.githubusercontent.com/u/222461688?s=60" width="40" height="40" alt="@TuanLe-bk18" title="@TuanLe-bk18 Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>&nbsp;
  <a href="https://github.com/uiYzzi"><img src="https://avatars.githubusercontent.com/u/40852301?s=60" width="40" height="40" alt="@uiYzzi" title="@uiYzzi Contributor (1 commit)" style="border-radius: 50%; border: 2px solid #B19CD9;" /></a>
</div>

</details>

## Support

### Sponsorship

VT Code is built and maintained in spare time. If it helped you ship or learn
something, a [sponsorship](https://github.com/sponsors/vinhnx) keeps the
project independent.

<div align="center">
  <a href="https://github.com/dnhn"><img src="https://avatars.githubusercontent.com/u/2561973" width="80" height="80" alt="@dnhn" style="border-radius: 50%" /></a>
  <a href="https://github.com/codemod"><img src="https://avatars.githubusercontent.com/u/78830094" width="80" height="80" alt="@codemod" style="border-radius: 50%" /></a>
  <a href="https://github.com/coderabbitai"><img src="https://avatars.githubusercontent.com/u/132028505" width="80" height="80" alt="@coderabbitai" style="border-radius: 50%" /></a>
  <a href="https://github.com/KhaiRyth"><img src="https://avatars.githubusercontent.com/u/273723951" width="80" height="80" alt="@KhaiRyth" style="border-radius: 50%" /></a>
</div>

<div align="center">

[![GitHub Sponsors](https://img.shields.io/badge/Sponsor-30363D?style=for-the-badge&logo=github-sponsors&logoColor=%23EA4AAA)](https://github.com/sponsors/vinhnx)
<a href="https://buymeacoffee.com/vinhnx"><img src="./resources/screenshots/qr_donate.png" alt="Buy Me a Coffee" width="100" /></a>

</div>

## License

First-party code is **MIT OR Apache-2.0**. See [LICENSE](LICENSE).
Third-party code keeps its original licenses: see [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES).

<div align="right">

[Back to top](#contents)

</div>
