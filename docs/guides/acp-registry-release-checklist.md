# ACP Registry Release Checklist

This checklist covers the exact steps to publish Diapason assets that pass
`agentclientprotocol/registry` validation.

It is a procedure for a future Diapason release. No Diapason release assets are
currently verified as published; do not add registry URLs until a release has
actually been built and uploaded.

## Preconditions

1. Merge ACP auth-method changes into `main`.
2. Confirm `crates/codegen/diapason-acp/src/zed/agent/handlers.rs` returns
   `authMethods` in `initialize`.
3. Ensure GitHub CLI is authenticated with permissions: `repo`, `workflow`.

## 1) Build and publish a macOS asset (local)

Run from repo root:

```bash
cargo build --release -p diapason
mkdir -p dist

tar -C target/release -czf dist/diapason-<VERSION>-aarch64-apple-darwin.tar.gz diapason
shasum -a 256 dist/diapason-<VERSION>-aarch64-apple-darwin.tar.gz > dist/checksums.txt
cut -d' ' -f1 dist/checksums.txt > dist/diapason-<VERSION>-aarch64-apple-darwin.sha256
```

Create or reuse the GitHub release and upload files:

```bash
gh release create <VERSION> \
  --repo leynos/vtcode \
  --target <COMMIT_SHA> \
  --title <VERSION> \
  --notes "Release <VERSION>"

gh release upload <VERSION> \
  --repo leynos/vtcode \
  dist/diapason-<VERSION>-aarch64-apple-darwin.tar.gz \
  dist/diapason-<VERSION>-aarch64-apple-darwin.sha256 \
  dist/checksums.txt \
  --clobber
```

## 2) Build Linux assets (GitHub Actions)

Trigger Linux build workflow for the same tag:

```bash
gh workflow run build-linux-windows.yml --repo leynos/vtcode --ref main \
  -f tag=<VERSION> \
  -f build_windows=false
```

Find latest run ID and watch it:

```bash
gh run list --repo leynos/vtcode --workflow build-linux-windows.yml \
  --limit 1 --json databaseId,status,conclusion,url

gh run watch <RUN_ID> --repo leynos/vtcode --exit-status
```

Download artifacts and upload to the release:

```bash
mkdir -p /tmp/diapason-linux-assets
cd /tmp/diapason-linux-assets

gh run download <RUN_ID> --repo leynos/vtcode

gh release upload <VERSION> --repo leynos/vtcode \
  artifacts-x86_64-unknown-linux-gnu/diapason-<VERSION>-x86_64-unknown-linux-gnu.tar.gz \
  artifacts-x86_64-unknown-linux-gnu/diapason-<VERSION>-x86_64-unknown-linux-gnu.sha256 \
  artifacts-x86_64-unknown-linux-musl/diapason-<VERSION>-x86_64-unknown-linux-musl.tar.gz \
  artifacts-x86_64-unknown-linux-musl/diapason-<VERSION>-x86_64-unknown-linux-musl.sha256 \
  --clobber
```

## 3) (Optional) Build Windows asset

Trigger with Windows enabled:

```bash
gh workflow run build-linux-windows.yml --repo leynos/vtcode --ref main \
  -f tag=<VERSION> \
  -f build_windows=true
```

When finished, upload Windows zip and checksum from workflow artifacts to the
same release.

## 4) Update ACP registry entry

Edit `agentclientprotocol/registry/diapason/agent.json`:

- set `version` to `<VERSION>`
- update existing archive URLs to `<VERSION>`
- add new `binary` targets only for published assets

Example target block:

```json
"linux-x86_64": {
  "archive": "https://github.com/leynos/vtcode/releases/download/<VERSION>/diapason-<VERSION>-x86_64-unknown-linux-gnu.tar.gz",
  "cmd": "./diapason",
  "args": ["acp"],
  "env": {
    "DIAPASON_ACP_ENABLED": "1",
    "DIAPASON_ACP_ZED_ENABLED": "1"
  }
}
```

## 5) Validate registry requirements locally

Run in `agentclientprotocol/registry`:

```bash
uv run --with jsonschema .github/workflows/build_registry.py
python3 .github/workflows/verify_agents.py --agent diapason --auth-check --clean
```

## 6) Open registry PR

```bash
git checkout -b add-diapason-agent-<VERSION>
git add diapason/agent.json diapason/icon.svg
git commit -m "Update Diapason registry entry to <VERSION>"
git push -u <your-fork-remote> add-diapason-agent-<VERSION>

gh pr create --repo agentclientprotocol/registry \
  --head <your-user>:add-diapason-agent-<VERSION> \
  --base main \
  --title "Update Diapason agent entry to <VERSION>"
```
