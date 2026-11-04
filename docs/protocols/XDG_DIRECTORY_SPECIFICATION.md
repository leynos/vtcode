# Diapason user directories

Diapason resolves user-level files using the XDG Base Directory Specification
on Linux and BSD, and native application-data conventions on macOS and Windows.
The main user configuration lives in the config category; durable runtime state
lives in the state category. Repository-local data uses `.diapason/` by default
and can be configured with `DIAPASON_WORKSPACE_DIR`.

Diapason does not read or migrate the historical `.vtcode/` or `~/.diapason/`
directories. See the
[user data directories guide](../guides/user-data-directories.md) for setup
details.

## Directory categories

On Linux and BSD, defaults are relative to the user's home directory:

| Category   | Environment variable | Default                     | Typical contents                                     |
| ---------- | -------------------- | --------------------------- | ---------------------------------------------------- |
| Config     | `XDG_CONFIG_HOME`    | `~/.config/diapason`        | `config.toml`, `AGENTS.md`, rules, provider settings |
| Data       | `XDG_DATA_HOME`      | `~/.local/share/diapason`   | Installed plugins and skills, durable assets         |
| State      | `XDG_STATE_HOME`     | `~/.local/state/diapason`   | Logs, audit records, sessions and persistent state   |
| Cache      | `XDG_CACHE_HOME`     | `~/.cache/diapason`         | Re-creatable caches, web fetches and output spools   |
| Runtime    | `XDG_RUNTIME_DIR`    | `$XDG_RUNTIME_DIR/diapason` | Transient sockets, locks and process coordination    |
| Executable | `XDG_BIN_HOME`       | `~/.local/bin`              | Managed helper binaries                              |

`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, and `XDG_CACHE_HOME`
select the respective category roots. Empty or relative XDG values are ignored.
`DIAPASON_CONFIG_PATH` or `--config /path/to/file.toml` selects an explicit
configuration file for a run. Only the standard XDG directory variables select
the category roots. The canonical user config file is `config.toml`.

System configuration candidates may be supplied through `XDG_CONFIG_DIRS`;
system directory lists accept only absolute entries. Configuration precedence
and explicit-file behaviour are documented in
[Configuration precedence](../config/CONFIGURATION_PRECEDENCE.md).

## Repository-local data

The default repository data directory is `.diapason/`. Set
`DIAPASON_WORKSPACE_DIR` to a different single relative directory name when a
repository needs another location. Absolute paths, empty values, traversal and
the reserved legacy `.vtcode` name are rejected.

Workspace data includes project instructions and workspace-specific support
files. Logs, audit records and session records use the user state directory so
they remain available independently of the current repository.

## Platform roots

- Linux and BSD follow the XDG categories above.
- macOS uses native Application Support and cache roots with a separate state
  location.
- Windows uses native application-data roots, separated into config, data,
  state and cache categories.

Use `diapason --version` to inspect the resolved paths for the current system.

## Permissions

New user directories are created privately. Authentication files and other
sensitive state use restrictive permissions on Unix. Existing data outside the
canonical Diapason locations is left untouched.

## References

- [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/)
- [Configuration precedence](../config/CONFIGURATION_PRECEDENCE.md)
- [User data directories guide](../guides/user-data-directories.md)
- [Security model](../development/COMMAND_SECURITY_MODEL.md)
