# Diapason Troubleshooting Guide

This guide covers common issues and solutions when using Diapason with your IDE.

## Prerequisites Not Found

**Issue**: Diapason extension can't find the Diapason CLI.

**Solution**:

1. Ensure Diapason CLI is installed:

    ```bash
    # Follow docs/installation/README.md to install from source.
    diapason --version
    ```

2. Check that Diapason is in your PATH:

    ```bash
    diapason --version
    ```

3. If Diapason is installed in a custom location, update your IDE settings to
   point to the correct path:
    - VS Code: Set `diapason.commandPath` in settings to the full path of the
      Diapason executable
    - Cursor/Windsurf: Look for similar extension settings to specify the
      Diapason executable path

## Extension Not Working

**Issue**: Diapason commands are not responding or showing errors.

**Solution**:

1. Restart your IDE after installing the extension
2. Check that your workspace contains a `config.toml` configuration file
3. Verify the extension is enabled in your IDE
4. Check the IDE's output panel for error messages

## AI Provider Not Working

**Issue**: Diapason can't connect to AI providers (OpenAI, Anthropic, etc.).

**Solution**:

1. Ensure you have valid API keys in your `config.toml` configuration file
2. Check that your API key has sufficient permissions
3. Verify your internet connection
4. Check if the AI provider has any service interruptions

## Slow Performance

**Issue**: Diapason is taking a long time to respond or analyse code.

**Solution**:

1. For large codebases, consider excluding large directories in your
   `config.toml`
2. Check that your system has sufficient memory and CPU resources
3. Ensure your internet connection is stable if using cloud-based AI providers
4. Consider switching to a faster AI model in your configuration

## Configuration Issues

**Issue**: Diapason isn't using expected configuration settings.

**Solution**:

1. Verify your `config.toml` file is in the root of your workspace
2. Check the syntax of your configuration file
3. Restart your IDE after making configuration changes
4. Use the "Diapason: Open Configuration" command to edit your config directly

## VS Code-Compatible Editors

**Issue**: Using Diapason with Cursor, Windsurf, or other VS Code-compatible
editors.

**Solution**: Diapason does not currently have a published marketplace listing.
Use the local extension from the repository checkout:

1. Ensure the Diapason CLI is installed separately on your system
2. Run or package `extensions/vscode-extension` using its
   [development guide](../../extensions/vscode-extension/DEVELOPMENT.md)
3. In Cursor or Windsurf, use **Install from VSIX** for a locally built package,
   or run the extension in its development host
4. See the [Cursor and Windsurf setup guide](./cursor-windsurf-setup.md) for
   details

## Need More Help?

If you're still experiencing issues:

1. Check the [main documentation](../README.md)
2. Review the [Cursor and Windsurf Setup Guide](./cursor-windsurf-setup.md) for
   editor-specific instructions
3. Open an issue on the
   [Diapason GitHub repository](https://github.com/leynos/vtcode/issues)
4. Provide detailed information about your setup, the issue you're
   experiencing, and any error messages
