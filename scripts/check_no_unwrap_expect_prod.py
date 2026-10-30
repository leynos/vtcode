#!/usr/bin/env python3
"""Warn or enforce against unwrap/expect usage in non-test Rust code."""

from __future__ import annotations

import argparse
import subprocess
import sys
from dataclasses import dataclass
from fnmatch import fnmatch
from pathlib import Path


# Direct importlib loading of this entry point uses the same local package as CLI execution.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from rust_panic_scanner.attributes import (
    AttributeGroup as AttributeGroup,
    ModuleDeclaration as ModuleDeclaration,
    RustAttribute as RustAttribute,
    SourceSpan as SourceSpan,
)
from rust_panic_scanner.lexer import RustLexer as RustLexer, RustToken as RustToken
from rust_panic_scanner.ownership import (
    ModuleBody as ModuleBody,
    ModuleGraph as _OwnershipGraph,
    ModulePaths as ModulePaths,
    ModuleReference as ModuleReference,
    ModuleResolver as ModuleResolver,
    ModuleScope as ModuleScope,
)
from rust_panic_scanner.syntax import RustSource


REPO_ROOT = Path(__file__).resolve().parent.parent
TEST_ROOT_DIRECTORIES = ("fuzz", "target", "tests")
NESTED_TEST_DIRECTORIES = ("tests", "benches", "examples")
TEST_SOURCE_SUFFIXES = ("_test.rs", "_tests.rs")


@dataclass
class AllowRule:
    """A reviewed path-pattern exception, optionally limited to one source line.

    Attributes
    ----------
    pattern : str
        Repository-relative fnmatch pattern, including legacy crate spellings.
    line : int or None
        One-based line to allow, or None for all lines matched by the pattern.
    """

    pattern: str
    line: int | None = None


def parse_args() -> argparse.Namespace:
    """Parse the existing warn/enforce CLI and report-limit options.

    Returns
    -------
    argparse.Namespace
        Validated mode, allowlist path and maximum number of printed findings.
    """
    parser = argparse.ArgumentParser(
        description="Validate production Rust code does not use unwrap()/expect().",
    )
    parser.add_argument(
        "--mode",
        choices=("warn", "enforce"),
        default="warn",
        help="warn: report only; enforce: fail on findings.",
    )
    parser.add_argument(
        "--allowlist",
        type=Path,
        default=Path("scripts/zen_allowlist.txt"),
        help="Allowlist file with path or path:line patterns (default: scripts/zen_allowlist.txt).",
    )
    parser.add_argument(
        "--max-findings",
        type=int,
        default=200,
        help="Maximum number of findings to print (default: 200).",
    )
    return parser.parse_args()


def list_tracked_rust_files() -> list[Path]:
    """List tracked Rust files using Git in the current repository root.

    Returns
    -------
    list of pathlib.Path
        Repository-rooted file paths in Git's reported order.

    Raises
    ------
    subprocess.CalledProcessError
        If Git cannot enumerate tracked files; enforcement must not scan a subset.
    """
    proc = subprocess.run(
        ["git", "ls-files", "-z", "*.rs"],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
    )
    raw = proc.stdout.decode("utf-8", errors="ignore")
    return [REPO_ROOT / Path(rel) for rel in raw.split("\0") if rel]


def is_test_path(path: Path) -> bool:
    """Apply the established filename policy without inferring Rust cfg values.

    Parameters
    ----------
    path : pathlib.Path
        File rooted in the current REPO_ROOT spelling.

    Returns
    -------
    bool
        Whether the existing test, example, bench or fuzz path policy excludes it.
    """
    rel = path.relative_to(REPO_ROOT).as_posix()
    path_parts = rel.split("/")
    return (
        _is_test_tree_path(path_parts)
        or _is_test_module_path(path_parts)
        or _is_test_source_path(path_parts[-1])
    )


def _is_test_tree_path(path_parts: list[str]) -> bool:
    has_workspace_test_root = (
        len(path_parts) > 1 and path_parts[0] in TEST_ROOT_DIRECTORIES
    )
    has_nested_test_directory = any(
        directory in NESTED_TEST_DIRECTORIES for directory in path_parts[1:-1]
    )
    return has_workspace_test_root or has_nested_test_directory


def _is_test_module_path(path_parts: list[str]) -> bool:
    return any(part.endswith("_tests") for part in path_parts[:-1])


def _is_test_source_path(filename: str) -> bool:
    return filename == "tests.rs" or filename.endswith(TEST_SOURCE_SUFFIXES)


def _is_test_target(path: Path) -> bool:
    # The graph canonicalises paths; restore the entry root spelling for policy.
    relative = path.relative_to(REPO_ROOT.resolve())
    return is_test_path(REPO_ROOT / relative)


class ModuleGraph(_OwnershipGraph):
    """Bind module ownership to the entry point's current repository root.

    Parameters
    ----------
    files : list of pathlib.Path
        Tracked Rust paths. Returned test-only paths are always canonical.

    Notes
    -----
    The filename policy is evaluated against the current REPO_ROOT, including
    when a caller monkeypatches it or uses a symlink spelling of that root.
    """

    def __init__(self, files: list[Path]) -> None:
        super().__init__(files, is_test_path=_is_test_target)


def parse_allow_rules(path: Path) -> list[AllowRule]:
    """Read reviewed path or path:line allowlist entries.

    Parameters
    ----------
    path : pathlib.Path
        Absolute path, or path relative to the current REPO_ROOT.

    Returns
    -------
    list of AllowRule
        Parsed non-comment entries; a missing allowlist supplies no exceptions.

    Raises
    ------
    OSError
        If an existing allowlist cannot be read.
    """
    if not path.is_absolute():
        path = REPO_ROOT / path
    if not path.exists():
        return []

    rules: list[AllowRule] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue

        token = line.split("|", 1)[0].strip()
        if not token:
            continue

        if ":" in token:
            prefix, _, suffix = token.rpartition(":")
            if suffix.isdigit() and prefix:
                rules.append(AllowRule(pattern=prefix, line=int(suffix)))
                continue

        rules.append(AllowRule(pattern=token, line=None))

    return rules


def is_allowlisted(rel_path: str, line_no: int, rules: list[AllowRule]) -> bool:
    """Check a source position against current and legacy crate path patterns.

    Parameters
    ----------
    rel_path : str
        Repository-relative POSIX path.
    line_no : int
        One-based original source line.
    rules : list of AllowRule
        Reviewed exceptions, applied without changing their line scope.

    Returns
    -------
    bool
        Whether at least one path-and-line rule allows this position.
    """
    candidates = {rel_path}
    if rel_path.startswith("crates/"):
        parts = rel_path.split("/", 2)
        if len(parts) > 2:
            candidates.add(parts[2])
    for rule in rules:
        for candidate in candidates:
            if not fnmatch(candidate, rule.pattern):
                continue
            if rule.line is None or rule.line == line_no:
                return True
    return False


def test_only_files(files: list[Path]) -> set[Path]:
    """Find external files proven to have only test code owners.

    Parameters
    ----------
    files : list of pathlib.Path
        Tracked Rust sources in the current REPO_ROOT.

    Returns
    -------
    set of pathlib.Path
        Canonical resolved paths with test ownership and no possible production
        route. Unknown paths and shared production use retain enforcement.

    Raises
    ------
    OSError
        If any source needed to establish ownership cannot be read.
    """
    return ModuleGraph(files).test_only()


def scan_file(path: Path, rules: list[AllowRule]) -> list[tuple[str, int, str]]:
    """Report locally production-reachable unwrap/expect source positions.

    Parameters
    ----------
    path : pathlib.Path
        Existing Rust file under REPO_ROOT.
    rules : list of AllowRule
        Reviewed path/line exceptions.

    Returns
    -------
    list of tuple
        Repository-relative path, one-based line and stripped source text per
        finding. Local test scopes and literal/comment bait are excluded.

    Notes
    -----
    External ownership and filename exclusions belong to scan_files; callers
    scanning an individual file are intentionally given only local scope proofs.
    """
    rel_path = path.relative_to(REPO_ROOT).as_posix()
    source = RustSource.read(path)
    lines = source.source.splitlines()
    findings: list[tuple[str, int, str]] = []
    for line_no in source.production_call_lines():
        if not is_allowlisted(rel_path, line_no, rules):
            findings.append((rel_path, line_no, lines[line_no - 1].strip()))
    return findings


@dataclass(frozen=True)
class ScanResult:
    """Aggregate of the files enforced and their retained source findings.

    Attributes
    ----------
    scanned_files : int
        Number of files remaining after filename and ownership exclusions.
    findings : list of tuple
        Ordered repository-relative path, one-based line and source text triples.
    """

    scanned_files: int
    findings: list[tuple[str, int, str]]


def scan_files(files: list[Path], rules: list[AllowRule]) -> ScanResult:
    """Apply filename, ownership, local-scope and allowlist policies in order.

    Parameters
    ----------
    files : list of pathlib.Path
        Complete tracked Rust input. Ownership proofs depend on the whole set.
    rules : list of AllowRule
        Reviewed exceptions; an empty list enforces every production position.

    Returns
    -------
    ScanResult
        Count and ordered findings without truncation or console side effects.

    Raises
    ------
    OSError
        If a required input cannot be read; no incomplete result is returned.
    """
    excluded = test_only_files(files)
    production = [path for path in files if not is_test_path(path)]
    production = [path for path in production if path.resolve() not in excluded]
    findings = [finding for path in production for finding in scan_file(path, rules)]
    return ScanResult(len(production), findings)


def report_findings(findings: list[tuple[str, int, str]], limit: int) -> None:
    """Print the existing bounded finding report to standard output.

    Parameters
    ----------
    findings : list of tuple
        Ordered path, one-based line and source triples.
    limit : int
        Existing CLI slicing limit; omitted findings are counted in the report.
    """
    if not findings:
        return
    print("\nFindings (path:line | source):")
    for rel_path, line_no, line_source in findings[:limit]:
        print(f"- {rel_path}:{line_no} | {line_source}")
    if len(findings) > limit:
        print(f"- ... {len(findings) - limit} additional finding(s) not shown")


def report_policy(mode: str) -> int:
    """Explain a non-empty finding set and return its CLI policy status.

    Parameters
    ----------
    mode : {"warn", "enforce"}
        Already validated CLI mode. Call only when findings exist.

    Returns
    -------
    int
        1 for enforce, with remediation on stderr; 0 for warning-only output.
    """
    if mode == "enforce":
        print(
            "\nunwrap/expect enforcement failed.\n\n"
            "Remediation:\n"
            "1. Replace unwrap()/expect() with anyhow::Result + with_context() in production code.\n"
            "2. If an exception is intentional, add a scoped entry to scripts/zen_allowlist.txt with rationale.\n"
            "3. Re-run: python3 scripts/check_no_unwrap_expect_prod.py --mode enforce --allowlist scripts/zen_allowlist.txt",
            file=sys.stderr,
        )
        return 1
    print(
        "\nWarning mode: findings reported but not blocking. "
        "Promote to --mode enforce after cleanup/allowlisting."
    )
    return 0


def main() -> int:
    """Run the tracked-source scan and print its policy report.

    Returns
    -------
    int
        Zero for clean or warning-only scans; one for enforce-mode findings.
    """
    args = parse_args()
    result = scan_files(list_tracked_rust_files(), parse_allow_rules(args.allowlist))
    print(
        f"unwrap/expect scan summary: scanned_files={result.scanned_files}, "
        f"findings={len(result.findings)}, mode={args.mode}"
    )
    report_findings(result.findings, args.max_findings)
    return report_policy(args.mode) if result.findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
