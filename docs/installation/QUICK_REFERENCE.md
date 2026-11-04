# Quick Reference

## Install

```bash
git clone https://github.com/leynos/vtcode.git diapason
cd diapason
cargo install --path .
```

## Quick Start

```bash
export OPENAI_API_KEY="sk-..."
diapason
```

## Uninstall

```bash
# Source install
cargo uninstall diapason
```

## Verify

```bash
diapason --version
```

## API Keys

```bash
export OPENAI_API_KEY="..."       # OpenAI
export ANTHROPIC_API_KEY="..."    # Anthropic
export GEMINI_API_KEY="..."       # Google Gemini
export MODEL_API_KEY="..."        # Meta AI (official, documented name)
# Or: export META_API_KEY="..."
export XAI_API_KEY="..."          # xAI
export DEEPSEEK_API_KEY="..."     # DeepSeek
export OPENROUTER_API_KEY="..."   # OpenRouter
export MERGE_GATEWAY_API_KEY="..." # Merge Gateway
```

## Authentication (OAuth / ChatGPT Subscription)

Diapason supports three ways to authenticate with OpenAI:

### 1. In-process ChatGPT OAuth (unofficial Codex compatibility flow)

No Codex CLI or app required — Diapason performs an in-process PKCE browser
login:

```bash
diapason login openai       # browser-based ChatGPT subscription login
diapason logout openai      # clear the stored Diapason session
diapason auth               # show auth status for all providers
```

Inside the TUI, use the equivalent slash commands:

```
/login openai    /logout openai    /auth
```

> **⚠️ Unofficial compatibility mechanism:** By default, Diapason reuses the
> Codex
> CLI's public PKCE client ID as an unofficial, unguaranteed compatibility
> approach. OpenAI has not documented or guaranteed third-party reuse of this
> client identity, and a public client ID is not authorization to reuse another
> tool's OAuth registration. Organizations with their own OpenAI-issued client
> pair can override via `DIAPASON_OPENAI_OAUTH_CLIENT_ID` /
> `DIAPASON_OPENAI_OAUTH_ORIGINATOR` (both must be set together).

### 2. Codex CLI fallback (automatic)

If you already ran `codex login`, Diapason automatically detects and uses
`~/.codex/auth.json` at runtime — no extra setup needed. To validate those
credentials explicitly without starting a new browser flow:

```bash
diapason login openai --from-codex   # validate Codex credentials (does not persist)
```

> **Note:** `--from-codex` validates and reuses Codex's runtime credentials but
> does **not** persist a separate Diapason session. Codex-owned tokens are not
> rotated by Diapason — copying or redeeming them could race Codex's refresh
> cycle or invalidate Codex-maintained credentials. Instead, Diapason re-reads
> `auth.json` when tokens need refreshing. Run `codex login` to refresh expired
> Codex tokens.

### 3. API key

```bash
export OPENAI_API_KEY="sk-..."
```

### Credential precedence

When `auth.openai.preferred_method = "auto"` (the default):

1. Diapason ChatGPT session from the in-process PKCE flow (full auto-refresh)
2. Codex `auth.json` fallback (managed by Codex)
3. `OPENAI_API_KEY` environment variable

Logout only clears Diapason's own session. To remove Codex fallback
credentials, run `codex logout` separately. See
[OAuth Authentication Guide](../guides/oauth-authentication.md) for details.

### Managing API-key secrets

```
/secret         # interactive secret manager (list, add, delete)
/secret list    # show which provider keys are stored (no values printed)
/secret add openai OPENAI_API_KEY
/secret delete openai OPENAI_API_KEY
```

Secrets are stored in the system keyring or encrypted file storage (see
`agent.credential_storage_mode` in config). OAuth sessions and API-key secrets
are separate — `/secret` manages API keys; `/login` manages OAuth.

## Resources

- Docs: <https://github.com/leynos/vtcode/tree/main/docs>
- Issues: <https://github.com/leynos/vtcode/issues>
- [Full Installation Guide](./README.md)
- [Historical VTCode installer reference](../archive/installation/NATIVE_INSTALLERS.md)
