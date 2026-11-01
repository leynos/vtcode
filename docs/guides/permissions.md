# Granular Permissions

VT Code uses explicit permission decisions instead of session permission
states. Primary agents and subagents carry their own local policy, and global
`[permissions]` rules can still provide workspace-wide ceilings and prompts.

## Agent Permission Schema

Every VT Code-native agent spec must provide `permissions.default`. Optional
rule buckets are evaluated in this order: `deny`, `ask`, `auto`, `allow`, then
`default`.

```yaml
permissions:
  default: ask
  allow:
    - exec_command
    - code_search
  ask:
    - write_stdin
  auto:
    - apply_patch
  deny:
    - rm
```

Allowed default decisions are `ask`, `allow`, `auto`, and `deny`.

`permissions.auto` means classifier-backed review. Matching tool calls are
reviewed by VT Code's permission reviewer; they are not treated as unrestricted
execution.

## Global Rules

The top-level `[permissions]` table in `vtcode.toml` defines workspace-wide
rule ceilings and prompts:

```toml
[permissions]
ask = ["exec_command(git commit *)", "apply_patch(/docs/**)"]
allow = ["exec_command", "code_search", "mcp__context7__*"]
deny = ["exec_command(rm -rf *)", "apply_patch(/.git/**)"]
```

Global rules do not define a default decision. The active primary agent or
active subagent supplies `permissions.default` after global deny and ask checks.

## Workspace Deletion

`exec_command` (and its PTY and pipe backends) accepts one simple `rm`
invocation only when the call also sets `confirm: true`:

```json
{ "cmd": "rm -f scratch/tmp.txt", "confirm": true }
```

The command shape and every resolved target are validated independently of the
permission decision. Chains, pipes, redirects, wrappers, environment prefixes,
globs, and shell expansion are refused; targets may not leave the workspace or
touch the workspace root, the home directory, or protected metadata directories
(`.git`, `.vtcode`, `.codex`, `.agents`). Global and agent deny rules still
win, so `deny = ["exec_command(rm -rf *)"]` continues to block the exact
`rm -rf` spelling, and the default deny regex blocks recursive spellings and
the long `--force` option in any flag order, including `rm -r` and `rm -fr`,
while the short `-f` remains allowed. Relaxing that regex permits the blocked
spellings; see the `deny_regex` guidance in
[the command security model](../development/COMMAND_SECURITY_MODEL.md).

Deletion cannot be typed into an already-running session: `write_stdin`,
`send_pty_input`, and command-session `write`/`continue` input are refused when
they name an `rm` invocation, because that path never reaches the executor gate.

A confirmed supported deletion still prompts in an ordinary session. Under
`--dangerously-skip-permissions` it is auto-approved by the runloop, because
the executor repeats the target assessment; unconfirmed and unsupported
deletions are still refused by command preflight. An ACP client without
skip-confirmations receives a normal permission request for the deletion rather
than a read-only auto-allow. See
[Command Security Model](../development/COMMAND_SECURITY_MODEL.md#workspace-scoped-deletion).

## Execution Policy and Skip-Permissions

Permission rules and the configured `[commands]` policy are separate gates.
Global and agent `allow`/`ask`/`auto`/`deny` rules decide whether a tool call
may proceed. `--full-auto`, `--skip-confirmations`, and
`--dangerously-skip-permissions` only auto-approve the promptable part of that
decision. They never relax the configured command policy, the curated command
validator, or mandatory sandbox restrictions.

A command denied by the configured policy is reported as:

```text
command '<program>' is not permitted by the execution policy: <category>;
--dangerously-skip-permissions does not override the command policy
```

The category names the rule collection that decided and its configured pattern,
or states that no allow rule matched:

- a `deny_list`, `deny_glob`, or `deny_regex` match names the collection and
  the configured pattern; deny rules win over every allow rule;
- an allowlist miss states that no `allow_list`, `allow_glob`, or `allow_regex`
  rule matches.

Diagnostics name only the program basename. Command arguments are never echoed,
so values passed to a command do not leak into logs or tool results.

When all three allow collections are empty, the configured policy falls back to
allowing unmatched commands. The shipped defaults configure allow rules, so an
unmatched command is denied. When a request carries both an argv and a distinct
raw shell source, both representations are evaluated and the command runs only
when both pass. Composition, aliases, wrappers such as `bash -lc`, and leading
environment prefixes do not inherit an exact match.

### GitHub CLI

The shipped defaults allow the local version queries `gh --version` and
`gh version`. The exception is an exact match: it does not extend to `gh api`,
`gh auth`, `gh alias`, `gh extension`, resource subcommands, or extra
arguments, and it cannot be combined with `&&`, `;`, `|`, or command
substitution. Every other `gh` invocation needs an explicit, narrow allow rule
in `[commands]`. The curated command validator accepts only those two forms for
`gh` and ignores `confirm` for that program.

## Classifier Review Settings

The classifier used by `permissions.auto` can be tuned from the permission
review settings:

```toml
[permissions.auto]
model = ""
probe_model = ""
max_consecutive_denials = 3
max_total_denials = 20
drop_broad_allow_rules = true
block_rules = []
allow_exceptions = []

[permissions.auto.environment]
trusted_paths = []
trusted_domains = []
trusted_git_hosts = []
trusted_git_orgs = []
trusted_services = []
```

After repeated classifier denials, VT Code falls back to manual prompts where
an interactive prompt is possible.

## Rule Grammar

`allow`, `ask`, `auto`, and `deny` accept exact VT Code tool ids and richer
rule grammar:

- `exec_command` or `exec_command(cargo check *)`
- `write_stdin`
- `apply_patch` or `apply_patch(/docs/**)`
- `code_search`
- `WebFetch(domain:example.com)`
- `mcp__context7`, `mcp__context7__*`, `mcp__context7__search-docs`
- Exact VT Code tool ids such as `exec_command` or `apply_patch`

## Path Matching

- `//path` matches an absolute filesystem path.
- `~/path` matches relative to the current user's home directory.
- `/path` matches relative to the workspace root.
- `path` or `./path` matches relative to the current working directory.

## Notes

- Global `deny` is a hard ceiling.
- Global `ask` can force a prompt even when the active agent would allow or
  auto-review a call.
- Agent-local `deny` wins within that agent's scope.
- `permissions.auto` uses classifier-backed review and does not bypass sandbox
  escalation prompts.
