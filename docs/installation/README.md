# Installation Guide

Diapason is currently installed from source. Packaged releases and third-party
package listings from the VTCode upstream are not Diapason distributions. The
native installer documents describe the upstream VTCode installer and are
retained for provenance; they do not install Diapason.

## Install from source

```bash
git clone https://github.com/leynos/vtcode.git diapason
cd diapason
cargo install --path .
```

The checkout includes its Cargo dependencies and build configuration. The
`cargo install --path .` command builds the binary locally; shell-specific
dependency setup is not performed by this command.

## After Installation

### 1. Verify it works

```bash
diapason --version
```

### 2. Set your API key

```bash
export OPENAI_API_KEY="sk-..."
```

### 3. Initialize your project

```bash
cd your-project
diapason init    # scaffolds config.toml, .diapason/, and AGENTS.md
```

### 4. Launch Diapason

```bash
diapason
```

## Supported AI Providers

- **GitHub Copilot** (requires `copilot` CLI; see
    [GitHub Copilot Auth](../guides/oauth-authentication.md#github-copilot-managed-auth))
- **OpenAI** (OPENAI_API_KEY)
- **Anthropic** (ANTHROPIC_API_KEY)
- **Google Gemini** (GEMINI_API_KEY)
- **Meta AI** (MODEL_API_KEY or META_API_KEY; see
    [Meta AI Provider](../providers/meta.md))
- **NVIDIA NIM** (NVIDIA_API_KEY; see
    [NVIDIA NIM Provider](../providers/nvidia.md))
- **Merge Gateway** (MERGE_GATEWAY_API_KEY; see
    [Merge Gateway Provider](../providers/merge-gateway.md))
- **xAI** (XAI_API_KEY)
- **DeepSeek** (DEEPSEEK_API_KEY)
- **OpenRouter** (OPENROUTER_API_KEY)
- **Ollama** (local, no API key)
- **LM Studio** (local, no API key by default)

Set the corresponding environment variable for your chosen provider. For GitHub
Copilot, install the `copilot` CLI and authenticate using `copilot login`.

## Troubleshooting

### Command not found after installation

**macOS/Linux:**

```bash
# Refresh your shell
source ~/.bashrc    # bash
source ~/.zshrc     # zsh
```

**Windows:** Restart PowerShell or Command Prompt.

### Source build fails

Check the Rust toolchain and build prerequisites in
[Development setup](../development/DEVELOPMENT_SETUP.md), then retry the local
build from the repository checkout.

### Permission denied

**macOS/Linux:**

```bash
chmod +x /usr/local/bin/diapason
```

**Windows:** Run PowerShell as Administrator.

### Download failed

1. Check internet connection: `curl https://api.github.com`
2. Verify GitHub is accessible
3. Try again in a fresh terminal
4. Check [GitHub status](https://www.githubstatus.com/)

### Still stuck?

- Open an issue: <https://github.com/leynos/vtcode/issues>
- Check docs: <https://github.com/leynos/vtcode/tree/main/docs>
- See
  [historical upstream installer notes](../archive/installation/NATIVE_INSTALLERS.md)

## Uninstall

### Native Installer (Shell)

```bash
rm /usr/local/bin/diapason
# or
rm ~/.local/bin/diapason
```

### Cargo

```bash
cargo uninstall diapason
```

### Windows (Uninstall)

```powershell
Remove-Item "$env:LOCALAPPDATA\Diapason\diapason.exe"
# or (if in Program Files)
Remove-Item "C:\Program Files\Diapason\diapason.exe"
```

## Installation Paths

### macOS & Linux Paths

- `/usr/local/bin/diapason` (standard)
- `/opt/local/bin/diapason` (Homebrew ARM64)
- `~/.local/bin/diapason` (user fallback)

### Windows Paths

- `C:\Program Files\Diapason\diapason.exe` (system-wide, requires admin)
- `%LOCALAPPDATA%\Diapason\diapason.exe` (user-scoped)

The native installers automatically select the best location and add it to PATH.

## Additional Resources

- **[Historical VTCode Installer Reference](../archive/installation/NATIVE_INSTALLERS.md)**
  - Upstream
    packaging background
- **[Quick Reference](./QUICK_REFERENCE.md)** - One-liner commands
- **[Diapason repository](https://github.com/leynos/vtcode)** - Source and
    published project documentation
- **[Documentation](https://github.com/leynos/vtcode/tree/main/docs)** - Full
    documentation
