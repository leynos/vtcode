# Cargo-Unmaintained

Guide for using `cargo-unmaintained` to detect unmaintained dependencies in VT Code.

## Overview

[`cargo-unmaintained`](https://github.com/trailofbits/cargo-unmaintained) is a Rust
tool that automatically finds unmaintained packages in Rust projects. It uses
heuristics to detect unmaintained packages by checking:

1. **Archived repository** - The package's repository is archived
2. **Not a repository member** - The package is not a member of its named repository
3. **Stale dependencies** - The package depends on a package whose latest version
   is incompatible and was released over a year ago, and the package either has
   no repository or its last commit was over a year ago

## Installation

```bash
cargo install cargo-unmaintained
```

## Usage

### Basic Scan

Run a scan on the entire workspace:

```bash
cd /path/to/vtcode
cargo unmaintained
```

### Verbose Output

Get detailed progress information:

```bash
cargo unmaintained --verbose
```

### JSON Output

For programmatic usage or CI integration:

```bash
cargo unmaintained --json
```

### Check Specific Package

Scan only a specific package:

```bash
cargo unmaintained --package vtcode-core
```

### Show Dependency Paths

See which dependencies bring in unmaintained packages:

```bash
cargo unmaintained --tree
```

## Configuration

### Classified baseline

CI runs `scripts/check_unmaintained_baseline.py`. It invokes the pinned
`cargo-unmaintained` release with JSON output, validates the documented output
shape, and compares every stable finding field with
`scripts/cargo_unmaintained_baseline.json`.

The baseline is tracked debt for [issue #108](https://github.com/leynos/vtcode/issues/108),
not an ignore list. Every entry records its exact package version, repository
status kind, stale dependency requirements, classification, and rationale.
The checker deliberately ignores only the changing age count. A new package,
version, status, requirement, malformed result, scanner error, or invalid
baseline fails CI.

Do not add packages to `[package.metadata.unmaintained].ignore` to pass this
check. Do not add `--no-exit-code`, `continue-on-error`, or an automated
baseline refresh. A missing previous finding is reported as eligible for
manual removal after it has been reviewed.

### GitHub Token (Optional)

To check if repositories are archived, you can set a GitHub token:

```bash
# Recommended: Path to file containing token
export GITHUB_TOKEN_PATH="$HOME/.github_token"

# Or: Direct token value (less secure)
export GITHUB_TOKEN="your_token_here"
```

Save token to config file:

```bash
cargo unmaintained --save-token
```

## Exit Codes

- `0` - No unmaintained packages found
- `1` - Unmaintained packages found
- `2` - Irrecoverable error occurred

## Common Options

- **Option:** `--color <WHEN>`
  - **Description:** Color output: `always`, `auto`, or `never` (default:
    `auto`)
- **Option:** `--fail-fast`
  - **Description:** Exit as soon as an unmaintained package is found
- **Option:** `--json`
  - **Description:** Output JSON (experimental)
- **Option:** `--max-age <DAYS>`
  - **Description:** Max age for repository commits (default: 365)
- **Option:** `--no-cache`
  - **Description:** Disable disk caching
- **Option:** `--no-exit-code`
  - **Description:** Don't set exit code on unmaintained packages
- **Option:** `--no-warnings`
  - **Description:** Suppress warnings
- **Option:** `-p, --package <NAME>`
  - **Description:** Check only a specific package
- **Option:** `--purge`
  - **Description:** Remove cached data and exit
- **Option:** `--tree`
  - **Description:** Show dependency paths to unmaintained packages
- **Option:** `--verbose`
  - **Description:** Show detailed progress information

## Integration with VT Code Development

### Pre-commit Check

Add to your pre-commit workflow to catch unmaintained dependencies early:

```bash
#!/bin/bash
# .git/hooks/pre-commit
python3 scripts/check_unmaintained_baseline.py
```

### CI/CD Integration

Add to your GitHub Actions workflow:

```yaml
- name: Check for unmaintained dependencies
  run: python3 scripts/check_unmaintained_baseline.py
```

### Periodic Audits

Run periodic audits to catch newly unmaintained packages:

```bash
# Monthly audit
cargo unmaintained --verbose --tree
```

## Troubleshooting

### GitHub API Rate Limits

If you see `401` or rate limit errors, set a GitHub token:

```bash
export GITHUB_TOKEN_PATH="$HOME/.github_token"
```

### Slow Scans

For large workspaces like VT Code, scans can take time. Use these options to
speed up:

```bash
# Disable caching for fresh scan
cargo unmaintained --no-cache

# Suppress warnings for cleaner output
cargo unmaintained --no-warnings
```

### False Positives

The scanner uses maintenance heuristics; an entry may need an upstream review,
but CI does not suppress it. If a finding is new or changes:

1. Inspect the package's lockfile and dependency path.
2. Check its repository and release metadata.
3. Open or update the tracking issue with the evidence.
4. Deliberately update the classified baseline only when the finding remains
   accepted debt.

## Example Output

```text
Scanning 632 packages and their dependencies
archival status of `some-crate` using GitHub API...ok (unarchived)
membership of `some-crate` using shallow clone...ok (member)
latest version of `another-crate` using crates.io index...ok (1.2.3)

`old-crate` appears to be unmaintained
  Repository: https://github.com/user/old-crate
  Last commit: 548 days ago
  Used by: vtcode-core -> dependency-chain -> old-crate
```

## Resources

- [cargo-unmaintained GitHub Repository](https://github.com/trailofbits/cargo-unmaintained)
- [crates.io page](https://crates.io/crates/cargo-unmaintained)
- [Trail of Bits Blog](https://www.trailofbits.com/)

## License

cargo-unmaintained is licensed under AGPLv3.
