#!/usr/bin/env python3
"""Check cargo-unmaintained output against the reviewed repository baseline.

The scanner's repository-status age is deliberately excluded from the identity
used by the ratchet.  Age changes on every scan and does not identify a new
maintenance finding.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
from typing import NoReturn, cast


REPO_ROOT = Path(__file__).resolve().parent.parent
BASELINE_PATH = REPO_ROOT / "scripts" / "cargo_unmaintained_baseline.json"

SCANNER_KEYS = frozenset({"name", "version", "repo_status", "outdated_deps"})
DEPENDENCY_KEYS = frozenset({"name", "req", "version_used", "version_latest"})
BASELINE_KEYS = frozenset({"schema_version", "repository", "entries"})
BASELINE_ENTRY_KEYS = SCANNER_KEYS | {"classification", "rationale", "issue"}
REPOSITORY = "leynos/vtcode"
SCHEMA_VERSION = 1
ISSUE_NUMBER = 108
REPOSITORY_STATUSES = frozenset(
    {"Uncloneable", "Unnamed", "Unassociated", "Nonexistent", "Archived"}
)


class ValidationError(ValueError):
    """Raised when scanner or baseline data violates its exact schema."""


def _invalid_json_constant(value: str) -> NoReturn:
    raise ValidationError(f"invalid JSON constant {value!r}")


def _load_json(text: str, source: str) -> object:
    try:
        return json.loads(text, parse_constant=_invalid_json_constant)
    except (json.JSONDecodeError, ValidationError) as error:
        raise ValidationError(f"invalid JSON from {source}: {error}") from error


def _require_string(value: object, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValidationError(f"{field} must be a non-empty string")
    return value


def _require_exact_keys(
    value: object, keys: frozenset[str], label: str
) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ValidationError(f"{label} must be an object")
    if frozenset(value) != keys:
        actual = sorted(value)
        expected = sorted(keys)
        raise ValidationError(f"{label} keys {actual!r} do not equal {expected!r}")
    return value


def _repo_status_identity(value: object, label: str) -> str:
    if isinstance(value, str):
        if value not in REPOSITORY_STATUSES:
            raise ValidationError(f"{label} has unknown repository status {value!r}")
        return value

    status = _require_exact_keys(value, frozenset({"Age"}), label)
    age = status["Age"]
    if isinstance(age, bool) or not isinstance(age, int) or age < 0:
        raise ValidationError(f"{label}.Age must be a non-negative integer")
    return "Age"


def _finding_identity(value: object, label: str) -> tuple[object, ...]:
    finding = _require_exact_keys(value, SCANNER_KEYS, label)
    name = _require_string(finding["name"], f"{label}.name")
    version = _require_string(finding["version"], f"{label}.version")
    status = _repo_status_identity(finding["repo_status"], f"{label}.repo_status")

    dependencies = finding["outdated_deps"]
    if not isinstance(dependencies, list):
        raise ValidationError(f"{label}.outdated_deps must be a list")

    dependency_identities: list[tuple[str, str, str, str]] = []
    for index, dependency in enumerate(dependencies):
        dependency_label = f"{label}.outdated_deps[{index}]"
        dependency_object = _require_exact_keys(
            dependency, DEPENDENCY_KEYS, dependency_label
        )
        dependency_identities.append(
            (
                _require_string(dependency_object["name"], f"{dependency_label}.name"),
                _require_string(dependency_object["req"], f"{dependency_label}.req"),
                _require_string(
                    dependency_object["version_used"],
                    f"{dependency_label}.version_used",
                ),
                _require_string(
                    dependency_object["version_latest"],
                    f"{dependency_label}.version_latest",
                ),
            )
        )

    return (name, version, status, tuple(sorted(dependency_identities)))


def _read_baseline_document() -> dict[str, object]:
    try:
        text = BASELINE_PATH.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValidationError(
            f"cannot read baseline {BASELINE_PATH}: {error}"
        ) from error

    return _require_exact_keys(
        _load_json(text, str(BASELINE_PATH)), BASELINE_KEYS, "baseline"
    )


def _validate_baseline_metadata(baseline: dict[str, object]) -> None:
    schema_version = baseline["schema_version"]
    if (
        isinstance(schema_version, bool)
        or not isinstance(schema_version, int)
        or schema_version != SCHEMA_VERSION
    ):
        raise ValidationError(f"baseline.schema_version must be {SCHEMA_VERSION}")
    if baseline["repository"] != REPOSITORY:
        raise ValidationError(f"baseline.repository must be {REPOSITORY!r}")


def _baseline_entries(baseline: dict[str, object]) -> list[object]:
    entries = baseline["entries"]
    if not isinstance(entries, list):
        raise ValidationError("baseline.entries must be a list")
    return entries


def _parse_baseline_entry(
    entry: object, index: int
) -> tuple[tuple[object, ...], dict[str, object]]:
    label = f"baseline.entries[{index}]"
    entry_object = _require_exact_keys(entry, BASELINE_ENTRY_KEYS, label)
    classification = _require_string(
        entry_object["classification"], f"{label}.classification"
    )
    rationale = _require_string(entry_object["rationale"], f"{label}.rationale")
    issue = entry_object["issue"]
    if isinstance(issue, bool) or not isinstance(issue, int) or issue != ISSUE_NUMBER:
        raise ValidationError(f"{label}.issue must be {ISSUE_NUMBER}")

    finding_fields = {key: entry_object[key] for key in SCANNER_KEYS}
    identity = _finding_identity(finding_fields, label)
    metadata = {
        "name": entry_object["name"],
        "version": entry_object["version"],
        "classification": classification,
        "rationale": rationale,
        "issue": issue,
    }
    return identity, metadata


def _index_baseline_entries(
    entries: list[object],
) -> dict[tuple[object, ...], dict[str, object]]:
    by_identity: dict[tuple[object, ...], dict[str, object]] = {}
    for index, entry in enumerate(entries):
        identity, metadata = _parse_baseline_entry(entry, index)
        if identity in by_identity:
            raise ValidationError(
                f"duplicate baseline finding identity at baseline.entries[{index}]"
            )
        by_identity[identity] = metadata
    return by_identity


def _load_baseline() -> dict[tuple[object, ...], dict[str, object]]:
    baseline = _read_baseline_document()
    _validate_baseline_metadata(baseline)
    return _index_baseline_entries(_baseline_entries(baseline))


def _run_scanner(scanner: str) -> subprocess.CompletedProcess[str]:
    command = [scanner, "unmaintained", "--json", "--no-warnings"]
    try:
        return subprocess.run(
            command,
            cwd=REPO_ROOT,
            capture_output=True,
            check=False,
            text=True,
        )
    except (OSError, UnicodeError) as error:
        raise ValidationError(f"could not launch scanner: {error}") from error


def _parse_scanner_output(
    completed: subprocess.CompletedProcess[str], scanner: str
) -> list[object]:
    try:
        output = _load_json(completed.stdout, f"scanner {scanner!r}")
    except ValidationError as error:
        stderr = completed.stderr.strip()
        detail = f" ({stderr})" if stderr else ""
        raise ValidationError(f"{error}{detail}") from error

    if not isinstance(output, list):
        raise ValidationError("scanner JSON top level must be a list")
    return output


def _validate_scanner_status(status: int, output: list[object]) -> None:
    if status not in (0, 1):
        raise ValidationError(f"scanner exited with unexpected status {status}")
    if (status == 0) != (not output):
        raise ValidationError(
            f"scanner status {status} contradicts finding count {len(output)}"
        )


def _load_scanner_output(scanner: str) -> tuple[int, list[object]]:
    completed = _run_scanner(scanner)
    output = _parse_scanner_output(completed, scanner)
    _validate_scanner_status(completed.returncode, output)
    return completed.returncode, output


def _validate_findings(
    findings: object,
) -> list[tuple[tuple[object, ...], dict[str, object]]]:
    if not isinstance(findings, list):
        raise ValidationError("scanner output must be a list")

    validated: list[tuple[tuple[object, ...], dict[str, object]]] = []
    seen: set[tuple[object, ...]] = set()
    for index, finding in enumerate(findings):
        label = f"scanner.findings[{index}]"
        identity = _finding_identity(finding, label)
        if identity in seen:
            raise ValidationError(f"duplicate scanner finding identity at {label}")
        seen.add(identity)
        validated.append((identity, cast(dict[str, object], finding)))
    return validated


def _parse_arguments(argv: list[str] | None) -> str | None:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--scanner", default="cargo")
    try:
        arguments = parser.parse_args(argv)
    except SystemExit:
        return None
    return arguments.scanner


def _load_current_state(
    scanner: str,
) -> tuple[
    list[tuple[tuple[object, ...], dict[str, object]]],
    dict[tuple[object, ...], dict[str, object]],
]:
    _scanner_status, raw_findings = _load_scanner_output(scanner)
    return _validate_findings(raw_findings), _load_baseline()


def _report_unexpected_findings(
    findings: list[tuple[tuple[object, ...], dict[str, object]]],
    baseline: dict[tuple[object, ...], dict[str, object]],
) -> bool:
    unexpected = [
        identity for identity, _finding in findings if identity not in baseline
    ]
    if not unexpected:
        return False

    for identity in unexpected:
        name, version, status, dependencies = identity
        print(
            f"error: unreviewed or changed finding {name}@{version} "
            f"(repository status {status}, {len(dependencies)} outdated dependencies)",
            file=sys.stderr,
        )
    return True


def _report_accepted_findings(
    findings: list[tuple[tuple[object, ...], dict[str, object]]],
    baseline: dict[tuple[object, ...], dict[str, object]],
) -> None:
    for identity, finding in findings:
        metadata = baseline[identity]
        print(
            f"accepted {finding['name']}@{finding['version']} "
            f"(issue #{metadata['issue']}, "
            f"classification: {metadata['classification']})"
        )


def _report_removable_baseline_entries(
    findings: list[tuple[tuple[object, ...], dict[str, object]]],
    baseline: dict[tuple[object, ...], dict[str, object]],
) -> None:
    current_identities = {identity for identity, _finding in findings}
    for identity, metadata in baseline.items():
        if identity not in current_identities:
            print(
                f"baseline entry eligible for manual removal: {metadata['name']}@{metadata['version']} "
                f"(issue #{ISSUE_NUMBER})"
            )


def main(argv: list[str] | None = None) -> int:
    scanner = _parse_arguments(argv)
    if scanner is None:
        print("usage: check_unmaintained_baseline.py [--scanner PATH]", file=sys.stderr)
        return 2

    try:
        findings, baseline = _load_current_state(scanner)
    except ValidationError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    if _report_unexpected_findings(findings, baseline):
        return 1

    _report_accepted_findings(findings, baseline)
    _report_removable_baseline_entries(findings, baseline)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
