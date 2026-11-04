# GitHub Copilot Managed Auth

Diapason uses the official `copilot` CLI for GitHub Copilot login and logout.
Diapason does not run a separate native GitHub OAuth flow for this provider.

## What You Need

- `copilot` in `PATH` for `diapason login copilot`, `diapason logout copilot`,
  `/login copilot`, and `/logout copilot`
- An active GitHub Copilot subscription
- Optional: `gh` in `PATH` if you want Diapason to detect an existing GitHub CLI
  auth session as a fallback

`gh` is not required for Copilot login/logout. Diapason only uses
`gh auth status` as an optional fallback when probing existing GitHub
authentication.

## If `copilot` Is Missing

Diapason will show a help note in the TUI transcript and in CLI auth output
when the configured Copilot command is not runnable.

Use one of these fixes:

- Install the official `copilot` CLI and make sure `copilot` is on `PATH`
- Point Diapason at a custom binary with `DIAPASON_COPILOT_COMMAND`
- Or configure a custom command in `config.toml`

```toml
[auth.copilot]
command = "/absolute/path/to/copilot"
```

After that, rerun one of:

```bash
diapason login copilot
diapason logout copilot
```

Or use the TUI slash commands:

```text
/login copilot
/logout copilot
```

## If `gh` Is Missing

Nothing special is required for Copilot login/logout. Diapason can still
authenticate through the official `copilot` CLI.

You only need `gh` if you want Diapason to reuse an existing GitHub CLI auth
session as a fallback during auth detection.

## Troubleshooting

- If `copilot` is installed but Diapason still cannot find it, check your shell
  `PATH`
- If Diapason uses the wrong binary, set `DIAPASON_COPILOT_COMMAND` or
  `[auth.copilot].command`
- If login succeeds in the standalone `copilot` CLI but Diapason still reports
  no auth, run `diapason auth status copilot` to inspect the detected auth
  source
