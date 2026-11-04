# Diapason Rust Extension for Zed

This repository includes the Diapason Rust editor extension for Zed. To run the
CLI as an ACP External Agent, add it as a custom agent in Zed's user-level
`settings.json`. Current
[Zed External Agents documentation](https://zed.dev/docs/ai/external-agents#custom-agents)
describes custom agents in settings and notes that extension-provided agents
are deprecated. Diapason is a df12 Productions hard fork of VTCode by Vinh
Nguyen, focused on ACP control and observability.

## Diapason Features in Zed

- Use Diapason from Zed's Agent panel through the custom ACP configuration
- Semantic code intelligence via Tree-sitter
- Support for multiple LLM providers (OpenAI, Anthropic, Google Gemini, etc.)
- Secure execution with workspace boundaries and tool policies
- Real-time streaming responses and reasoning

## Configure the ACP Agent

Install Diapason from source and ensure `diapason` is on `PATH`. Merge this
entry into Zed's user-level `settings.json`; it is separate from
`extension.toml`:

```json
{
  "agent_servers": {
    "Diapason": {
      "type": "custom",
      "command": "diapason",
      "args": ["acp"],
      "env": {
        "DIAPASON_ACP_ENABLED": "1",
        "DIAPASON_ACP_ZED_ENABLED": "1"
      }
    }
  }
}
```

Open Zed's Agent Settings and choose **Add Custom Agent**, or follow the
[current Zed External Agents documentation](https://zed.dev/docs/ai/external-agents#custom-agents).
Zed currently deprecates extension-provided agent servers; this Rust extension
manifest does not register the ACP agent.

## Building from Source

From the repository root, install the CLI with Cargo:

```bash
cargo install --path . --locked
```

To load the Rust editor extension during development, use Zed's **Install Dev
Extension** command and select `extensions/zed-extension`. Configure the CLI as
a custom ACP agent separately using the settings above.

## Development and Testing

For local development, build the CLI with `cargo build --release`, ensure
`target/release` is on `PATH` (or install with
`cargo install --path . --locked`), and install this directory as a Zed
development extension. Check Zed's ACP logs to verify the agent server is
communicating.

## Installation Troubleshooting

If you encounter "failed to install zed extensions" errors:

1. Confirm `diapason` is installed and available on `PATH`.
2. Use Zed's **Install Dev Extension** command to install this directory.
3. Check Zed's ACP logs if the agent server does not start.

## Requirements

- A Zed version with custom ACP External Agent support
- Diapason configuration with your AI provider settings
