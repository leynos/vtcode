# IDE Setup

Diapason does not currently have published editor marketplace listings or
downloadable extension packages. Install the CLI from this repository and use
the local editor extensions.

## Install the Diapason CLI

Follow the [source installation guide](../installation/README.md). From a
checkout, install the CLI with:

```bash
cargo install --path . --locked
```

The CLI must be available on your `PATH` before an editor extension can launch
it.

## Visual Studio Code, Cursor, and Windsurf

The VS Code extension can be run from a local checkout or packaged locally as a
VSIX. Open `extensions/vscode-extension` in VS Code and press **F5** to launch
an Extension Development Host. For a locally packaged VSIX, follow the
[extension README](../../extensions/vscode-extension/README.md) and
[development guide](../../extensions/vscode-extension/DEVELOPMENT.md). The
marketplace listing is not published; do not search a marketplace for a
Diapason listing.

## Zed

Install the local Zed development extension from the checkout: in Zed, choose
**Install Dev Extension** and select `extensions/zed-extension`. The
[Zed extension README](../../extensions/zed-extension/README.md) explains how
it connects to the local `diapason acp` process.

## Help

- [IDE troubleshooting](./troubleshooting.md)
- [Diapason documentation](../README.md)
- [Diapason GitHub issues](https://github.com/leynos/vtcode/issues)
