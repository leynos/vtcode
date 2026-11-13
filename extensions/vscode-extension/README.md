# Diapason

**Diapason** is an AI coding assistant available for your favourite IDEs. This
repository contains the Visual Studio Code extension, providing deep
integration with [Diapason](https://github.com/leynos/vtcode), a Rust-based
terminal coding agent with semantic code intelligence.

The Diapason Marketplace listing will be available after the extension is
published.

## Features

- **AI Coding Assistant**: Access the Diapason agent directly from VS Code
- **Quick Actions**: Easily send questions and get responses without leaving
    your editor
- **Code Analysis**: Analyse your workspace with semantic code intelligence
- **Configuration Management**: Edit your `config.toml` configuration files
    with syntax highlighting
- **Context Awareness**: Leverages Tree-sitter for deep code understanding
    with enhanced regex search
- **Multi-Provider AI**: Supports OpenAI, Anthropic, Google, xAI, DeepSeek,
    and more
- **Security First**: Built-in safeguards with human-in-the-loop controls

## Prerequisites

Before using this extension, you need to have the Diapason CLI installed:

```bash
# From the repository root, install with Cargo:
cargo install --git https://github.com/leynos/vtcode.git --branch main --locked diapason

# Or build the Homebrew formula from the repository root:
brew install --HEAD ./homebrew/diapason.rb
```

## Quick Start

1. Install the Diapason CLI from source or select a local binary
2. Build and install the extension locally from this repository; the Diapason
   Marketplace listing has not been published
3. Open a workspace containing a `config.toml` file
4. Access Diapason features through:
    - The Command Palette (`Cmd+Shift+P` or `Ctrl+Shift+P`)
    - The Diapason Quick Actions view in the Explorer
    - Right-click context menu on selected code
    - Status bar icon

## Commands

The extension contributes the following commands:

- `Diapason: Open Quick Actions` - Access the quick actions panel
- `Diapason: Ask the Agent` - Send a question to the Diapason agent
- `Diapason: Ask About Selection` - Ask about highlighted code
- `Diapason: Launch Agent Terminal` - Open an integrated terminal session
    running `diapason chat`
- `Diapason: Analyse Workspace` - Run `diapason analyse` on your workspace
- `Diapason: Open Configuration` - Edit your `config.toml` configuration file
- `Diapason: Open Documentation` - Access Diapason documentation
- `Diapason: Toggle Human-in-the-Loop` - Control human approval for sensitive
    operations
- And more…

## Configuration

The extension contributes the following settings:

- `diapason.commandPath`: Path to the Diapason executable (default: `diapason`)

For comprehensive Diapason configuration options, see the
[configuration documentation](./docs/config.md). You can also use the example
configuration example:

- [config.toml.example](./config.toml.example) - Comprehensive configuration
    with all options

## Requirements

- VS Code version 1.87.0 or higher
- Diapason now supports placing the Diapason view container in VS Code's
    Secondary Side Bar (VS Code 1.106+). You can move the Diapason container
    from the activity bar to the secondary side bar via drag-and-drop or
    through the context menus in the UI.
- Diapason CLI installed and accessible in your PATH

## Contributing

Contributions are welcome! Please see the
[main Diapason repository](https://github.com/leynos/vtcode) for contribution
guidelines.

### Development

For development instructions, see [DEVELOPMENT.md](DEVELOPMENT.md).

### Releasing

To release a new version of the extension:

```bash
# Patch release (0.1.1 -> 0.1.2)
./release.sh patch

# Minor release (0.1.1 -> 0.2.0)
./release.sh minor

# Major release (0.1.1 -> 1.0.0)
./release.sh major
```

The release script automates version bumping, building, packaging, and
publishing to the VS Code Marketplace and Open VSX Registry. No extension
listing is currently published. See [RELEASE.md](RELEASE.md) for details.

## Support

If you find Diapason useful, please consider supporting the project by visiting
[BuyMeACoffee](https://www.buymeacoffee.com/vinhnx).

## License

This extension is licensed under **MIT OR Apache-2.0**. See
[LICENSE.txt](LICENSE.txt) for the full Apache-2.0 text; MIT terms are also
granted under the same copyright.
