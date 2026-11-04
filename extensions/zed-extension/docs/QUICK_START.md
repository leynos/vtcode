# Diapason Zed Extension - Quick Start

Use the Diapason CLI in Zed through a custom ACP External Agent.

## 1. Install Prerequisites

### Diapason CLI

Clone the repository and install Diapason from source:

```bash
git clone https://github.com/leynos/vtcode.git diapason
cd diapason
cargo install --path . --locked
```

Verify installation:

```bash
diapason --version
```

### Rust Toolchain

Cargo requires the Rust toolchain to build Diapason from source:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 2. Load the Rust Editor Extension (Optional)

The Rust editor extension is separate from the ACP agent configuration. To load
it during development, open Zed's Extensions panel, choose **Install Dev
Extension**, and select `extensions/zed-extension` from the `diapason` checkout.

## 3. Configure Diapason

Create a `config.toml` in your workspace root:

```toml
[ai]
provider = "openai"
model = "gpt5-nano"

[workspace]
analyze_on_startup = false
max_context_tokens = 8000

[security]
human_in_the_loop = true
```

### Configure Your AI Provider

Get API credentials from your chosen provider:

- **Anthropic**: [console.anthropic.com](https://console.anthropic.com)
- **OpenAI**: [platform.openai.com](https://platform.openai.com)
- **Google**: [aistudio.google.com](https://aistudio.google.com)

Set your API key (the CLI will prompt for it, or set the environment variable):

```bash
export ANTHROPIC_API_KEY="your-api-key"
# or
export OPENAI_API_KEY="your-api-key"
```

## 4. Configure the Custom ACP Agent

Open Zed's Agent Settings and add a custom agent in the user-level
`settings.json`. Merge this entry into the existing JSON:

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

See
[Zed's custom External Agent documentation](https://zed.dev/docs/ai/external-agents#custom-agents).
Zed currently deprecates extension-provided agent servers.

## 5. First Use

### Test Installation

1. Open a workspace with `config.toml`
2. Open Zed's Agent panel
3. Start a new thread and select **Diapason**

### Basic Workflow

Start a thread with Diapason from the Agent panel and ask a question. Diapason
uses the selected workspace and its `config.toml`.

## 6. Common Configuration Patterns

### Minimal Setup (OpenAI)

```toml
[ai]
provider = "openai"
model = "gpt-5"
```

### Full-Featured Setup

```toml
[ai]
provider = "anthropic"
model = "claude-sonnet-5"

[workspace]
analyze_on_startup = false
max_context_tokens = 8000
ignore_patterns = ["node_modules", ".git", "dist"]

[security]
human_in_the_loop = true
allowed_tools = ["read_file", "edit_file", "analyze"]

[llm]
temperature = 0.7
top_p = 0.9
```

### Development Mode (Local Testing)

```toml
[ai]
provider = "anthropic"
model = "claude-sonnet-5"

[workspace]
analyze_on_startup = false
log_level = "debug"
```

## Troubleshooting

### Diapason Is Unavailable

- **Issue**: Zed cannot start the Diapason agent
- **Solution**: Verify `diapason` is on `PATH` with `which diapason` or
  `diapason --version`

### Diapason Does Not Appear in the New-Thread Menu

- **Issue**: The custom agent is missing from the Agent panel
- **Solution**: Check the `agent_servers` entry in Zed user settings and reload
  Zed if needed

### API Key Errors

- **Issue**: "Invalid API key" or authentication errors
- **Solution**:
  - Check your `config.toml` provider configuration
  - Verify API key is set: `echo $ANTHROPIC_API_KEY` (for Anthropic)
  - Get a new API key from your provider's console

### Configuration Not Loading

- **Issue**: Settings not being applied
- **Solution**:
  - Ensure `config.toml` is in workspace root
  - Check file syntax (should be valid TOML)
  - Reload workspace (close/reopen folder in Zed)

## Next Steps

1. **Read Configuration Guide**:
   [Full configuration options](extension-features.md)
2. **Check Diapason Documentation**:
   [Main Diapason repo](https://github.com/leynos/vtcode)
3. **Join Community**: Star the repo and share feedback
4. **Contribute**: [Development guide](DEVELOPMENT.md)

## Support

- **GitHub Issues**: [Diapason Issues](https://github.com/leynos/vtcode/issues)
- **Documentation**:
  [Diapason Docs](https://github.com/leynos/vtcode#documentation)
- **Discord**: [Join our community](https://discord.com/invite/...)

## Tips & Tricks

### Pro Tips

1. **Keyboard Shortcuts**: Bind `agent: new external agent thread` to a
   keybinding
2. **Context**: The more context you provide, the better responses
3. **Iterations**: Use "Ask the Agent" for follow-up questions in a conversation
4. **Analysis**: Ask Diapason to analyse the workspace or selected code
5. **Config as Code**: Version control your `config.toml` in git

### Performance Tips

- Keep `max_context_tokens` reasonable (4000-8000)
- Exclude large directories in `ignore_patterns`
- Set `analyze_on_startup = false` for faster startup
- Use appropriate AI models (smaller for speed, larger for quality)

______________________________________________________________________

**Ready to start?** Configure Diapason as a custom ACP agent in Zed, create a
`config.toml`, and start a new thread.
