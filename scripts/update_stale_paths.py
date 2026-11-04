#!/usr/bin/env python3
"""Update stale crate paths from old flat layout to new crates/{common,codegen}/ layout."""

import os
import re

# Mapping: old prefix -> new prefix
# Only matching exact crate/src/... patterns to avoid false positives
REPLACEMENTS = [
    ("crates/codegen/diapason-core/src/", "crates/codegen/diapason-core/src/"),
    ("crates/common/diapason-commons/src/", "crates/common/diapason-commons/src/"),
    ("crates/codegen/diapason-config/src/", "crates/codegen/diapason-config/src/"),
    ("crates/codegen/diapason-ui/src/", "crates/codegen/diapason-ui/src/"),
    ("crates/codegen/diapason-indexer/src/", "crates/codegen/diapason-indexer/src/"),
    ("crates/codegen/diapason-bash-runner/src/", "crates/codegen/diapason-bash-runner/src/"),
    ("crates/common/diapason-exec-events/src/", "crates/common/diapason-exec-events/src/"),
    ("crates/codegen/diapason-acp/src/", "crates/codegen/diapason-acp/src/"),
    ("crates/codegen/diapason-auth/src/", "crates/codegen/diapason-auth/src/"),
    ("crates/codegen/diapason-a2a/src/", "crates/codegen/diapason-a2a/src/"),
    ("crates/codegen/diapason-mcp/src/", "crates/codegen/diapason-mcp/src/"),
    ("crates/codegen/diapason-llm/src/", "crates/codegen/diapason-llm/src/"),
    ("crates/codegen/diapason-safety/src/", "crates/codegen/diapason-safety/src/"),
    ("crates/codegen/diapason-memory/src/", "crates/codegen/diapason-memory/src/"),
    ("crates/codegen/diapason-skills/src/", "crates/codegen/diapason-skills/src/"),
    ("crates/codegen/diapason-eval/src/", "crates/codegen/diapason-eval/src/"),
]

# Directories to scan
SCAN_DIRS = [
    "crates",
    "docs",
    "scripts",
    ".github",
    "src",
    "tests",
]

# File extensions to process
EXTENSIONS = {
    ".rs",
    ".md",
    ".toml",
    ".py",
    ".sh",
    ".json",
}


def should_process(filepath: str) -> bool:
    _, ext = os.path.splitext(filepath)
    return ext in EXTENSIONS


def update_file(filepath: str, dry_run: bool = False) -> int:
    """Update stale paths in a file. Returns number of replacements made."""
    try:
        with open(filepath, "r", encoding="utf-8") as f:
            content = f.read()
    except Exception:
        return 0

    original = content
    for old, new in REPLACEMENTS:
        content = content.replace(old, new)

    if content != original:
        count = sum(original.count(old) for old, _ in REPLACEMENTS)
        if not dry_run:
            with open(filepath, "w", encoding="utf-8") as f:
                f.write(content)
        return count
    return 0


def main():
    dry_run = False
    total_files = 0
    total_replacements = 0

    for scan_dir in SCAN_DIRS:
        if not os.path.isdir(scan_dir):
            continue
        for root, dirs, files in os.walk(scan_dir):
            # Skip target directories
            if "target" in dirs:
                dirs.remove("target")
            # Skip .git
            if ".git" in dirs:
                dirs.remove(".git")
            # Skip .diapason runtime data
            if ".diapason" in dirs:
                dirs.remove(".diapason")

            for fname in files:
                filepath = os.path.join(root, fname)
                if should_process(filepath):
                    count = update_file(filepath, dry_run=dry_run)
                    if count > 0:
                        print(f"  {count:4d} replacements -> {filepath}")
                        total_files += 1
                        total_replacements += count

    print(
        f"\nTotal: {total_files} files updated, {total_replacements} replacements made."
    )


if __name__ == "__main__":
    main()
