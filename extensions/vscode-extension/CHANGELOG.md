# Change Log

All notable changes to the "diapason-companion" extension will be documented in
this file.

The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## VTCode provenance

Entries through version 0.50.14 record the original extension authored by Vinh
Nguyen. Diapason is a df12 Productions hard fork; those entries are retained as
upstream history.

## [Unreleased]

## [0.50.14] - 2026-05-21

### Removed

- Removed dead webview-based chat agent views (`ChatViewProvider`,
    `EnhancedChatViewProvider`) and their associated files (chat-view,
    enhanced-chat, mcp-chat-adapter, integration examples). The extension
    continues to use the VS Code native chat participant API.

## [0.50.11] - 2026-03-12

### Added

- Support for placing the Diapason view container in VS Code's Secondary Side
    Bar (VS Code 1.106+). This allows users to move the Diapason container into
    the secondary side bar for improved workspace layout.

## [0.42.18] - 2025-11-07

## [0.42.17] - 2025-11-07

## [0.42.16] - 2025-11-07

## [0.1.1] - 2025-11-03

### Added

- Initial release of Diapason extension
- Quick actions panel for easy access to Diapason features
- Status bar integration showing Diapason CLI availability
- Command palette integration with all Diapason commands
- Context menu integration for code selection
- Integrated terminal support for `diapason chat`
- Configuration file support for `config.toml`
- Syntax highlighting for Diapason configuration files
- Workspace analysis capabilities
- Human-in-the-loop controls
- MCP (Model Context Protocol) provider configuration
- Tool policy management
- Documentation and walkthrough integration

### Changed

### Fixed

### Removed
