# Diapason user data directories

Diapason keeps user configuration, user state and workspace data in separate
locations. On Linux and BSD, the application follows the XDG Base Directory
Specification. It does not read or migrate data from legacy `.vtcode/`
directories.

## User configuration

The configuration root is `$XDG_CONFIG_HOME/diapason/`, defaulting to
`~/.config/diapason/` when `XDG_CONFIG_HOME` is unset. The main configuration
file is `config.toml`; user agent instructions belong in `AGENTS.md` in the
same directory. Other user-level configuration, such as rules and provider
credentials, is stored below this root.

`XDG_CONFIG_HOME` selects the XDG root. `DIAPASON_CONFIG_PATH` can select an
explicit configuration file for the current session. The CLI `--config` option
also accepts a file path or an inline `key=value` override. Consult
[configuration precedence](../config/CONFIGURATION_PRECEDENCE.md) for layer
ordering and write behaviour.

## User state

Persistent, user-wide runtime state is stored below
`$XDG_STATE_HOME/diapason/`, defaulting to `~/.local/state/diapason/` when
`XDG_STATE_HOME` is unset. Logs, audit records and persisted cross-workspace
sessions belong here, alongside other durable runtime state. This keeps user
state separate from configuration and from any one repository.

The XDG variables `XDG_DATA_HOME` and `XDG_CACHE_HOME` select application data
and re-creatable cache roots. Their defaults are `~/.local/share/diapason/` and
`~/.cache/diapason/`, respectively. Platform-native equivalents apply on macOS
and Windows.

## Workspace data

Repository-local agent files are kept in `.diapason/` by default. Set
`DIAPASON_WORKSPACE_DIR` to another single relative directory name to change
that location for the current process. Absolute paths, path traversal and the
legacy `.vtcode` directory name are rejected.

The selected workspace directory contains project-local instructions and
workspace-specific support files. User-wide logs, audit records and persisted
sessions remain under the XDG state root.

## Legacy data

Diapason does not scan or migrate `.vtcode/` directories. Existing VTCode data
is left untouched. If you need information from an old tree, inspect and copy
the specific files you want to keep into the corresponding Diapason location
yourself; do not copy credentials or executable hooks without reviewing them.

For the application-level path API and configuration layering, see
[configuration precedence](../config/CONFIGURATION_PRECEDENCE.md) and
[configuration field reference](../config/CONFIG_FIELD_REFERENCE.md).
