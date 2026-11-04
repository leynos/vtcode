# Cursor and Windsurf Setup Guide

Diapason does not currently have a published marketplace listing or extension
download. Use the local VS Code compatible extension from a Diapason checkout.

## Prerequisites

1. Install the Diapason CLI using the
   [source installation guide](../installation/README.md).
2. Install Node.js and the editor tooling described in the
   [extension development guide](../../extensions/vscode-extension/DEVELOPMENT.md).

## Install the local extension

Open `extensions/vscode-extension` from the checkout in Cursor or Windsurf and
follow the [extension README](../../extensions/vscode-extension/README.md). For
local development, run the extension in an Extension Development Host. To
install a local package, build a VSIX as described in the development guide and
use the editor's **Install from VSIX** command. The local extension uses the
`diapason` executable on `PATH`; set `diapason.commandPath` if it is installed
elsewhere.

## Troubleshooting

- Confirm `diapason --version` works in a terminal available to the editor.
- Check the editor's extension output for launch errors.
- Ensure the workspace has the required Diapason configuration; see the
  [configuration guide](../config/config.md).
- See [IDE troubleshooting](./troubleshooting.md) or report an issue in the
  [Diapason repository](https://github.com/leynos/vtcode/issues).
