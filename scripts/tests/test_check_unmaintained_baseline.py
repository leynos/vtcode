#!/usr/bin/env python3
"""Behavioural tests for the cargo-unmaintained baseline ratchet."""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import stat
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest


CHECKER_SOURCE = Path(__file__).resolve().parents[1] / "check_unmaintained_baseline.py"


def finding(
    name: str = "known-package",
    version: str = "1.2.3",
    repo_status: object = "Unassociated",
    outdated_deps: list[dict[str, str]] | None = None,
) -> dict[str, object]:
    return {
        "name": name,
        "version": version,
        "repo_status": repo_status,
        "outdated_deps": outdated_deps if outdated_deps is not None else [],
    }


def baseline_entry(
    package: dict[str, object],
    classification: str = "repository-layout",
    rationale: str = "The scanner reports this package because its repository layout is not recognised.",
) -> dict[str, object]:
    return {
        **package,
        "classification": classification,
        "rationale": rationale,
        "issue": 108,
    }


def outdated_dependency(
    req: str = "^1.0",
    version_used: str = "1.0.0",
    version_latest: str = "2.0.0",
) -> dict[str, str]:
    return {
        "name": "dep",
        "req": req,
        "version_used": version_used,
        "version_latest": version_latest,
    }


class CheckerHarness:
    """Copy the checker into a temporary repository and run its real wrapper."""

    def __init__(
        self, scanner_output: str | bytes, scanner_status: int, baseline: object
    ) -> None:
        self.scanner_output = scanner_output
        self.scanner_status = scanner_status
        self.baseline = baseline
        self.directory = TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.invocations_path = self.root / "scanner-invocations"
        scripts = self.root / "scripts"
        scripts.mkdir()
        self.checker = scripts / "check_unmaintained_baseline.py"
        shutil.copy2(CHECKER_SOURCE, self.checker)
        self.checker.chmod(self.checker.stat().st_mode | stat.S_IXUSR)

        baseline_path = scripts / "cargo_unmaintained_baseline.json"
        if isinstance(baseline, str):
            baseline_path.write_text(baseline, encoding="utf-8")
        else:
            baseline_path.write_text(json.dumps(baseline), encoding="utf-8")

    def scanner(self, name: str = "fake-scanner") -> Path:
        scanner = self.root / name
        payload = (
            self.scanner_output.encode("utf-8")
            if isinstance(self.scanner_output, str)
            else self.scanner_output
        )
        script = (
            "#!/usr/bin/env python3\n"
            "from pathlib import Path\n"
            "import sys\n"
            f"with Path({str(self.invocations_path)!r}).open('ab') as invocations:\n"
            "    invocations.write(b'scan\\n')\n"
            f"sys.stdout.buffer.write({payload!r})\n"
            f"raise SystemExit({self.scanner_status})\n"
        )
        scanner.write_text(script, encoding="utf-8")
        scanner.chmod(scanner.stat().st_mode | stat.S_IXUSR)
        return scanner

    def run(
        self, scanner_name: str = "fake-scanner", capture_json: Path | None = None
    ) -> subprocess.CompletedProcess[str]:
        scanner = self.scanner(scanner_name)
        command = [sys.executable, str(self.checker), "--scanner", str(scanner)]
        if capture_json is not None:
            command.extend(["--capture-json", str(capture_json)])
        return subprocess.run(
            command,
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )

    def close(self) -> None:
        self.directory.cleanup()

    def __enter__(self) -> "CheckerHarness":
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


class CheckUnmaintainedBaselineTests(unittest.TestCase):
    def run_checker(
        self,
        scanner_output: object,
        scanner_status: int,
        baseline_entries: list[dict[str, object]] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        output = (
            scanner_output
            if isinstance(scanner_output, str)
            else json.dumps(scanner_output)
        )
        baseline = {
            "schema_version": 1,
            "repository": "leynos/vtcode",
            "entries": baseline_entries if baseline_entries is not None else [],
        }
        with CheckerHarness(output, scanner_status, baseline) as harness:
            return harness.run()

    def test_known_finding_is_accepted_and_printed_with_issue_and_classification(
        self,
    ) -> None:
        package = finding(
            repo_status={"Age": 999},
            outdated_deps=[
                {
                    "name": "dep",
                    "req": "^1.0",
                    "version_used": "1.0.0",
                    "version_latest": "2.0.0",
                }
            ],
        )
        baseline = baseline_entry({**package, "repo_status": {"Age": 377}})
        result = self.run_checker([package], 1, [baseline])

        self.assertEqual(result.returncode, 0)
        self.assertIn("accepted known-package@1.2.3", result.stdout)
        self.assertIn("issue #108", result.stdout)
        self.assertIn("classification: repository-layout", result.stdout)

    def test_age_value_is_not_part_of_finding_identity(self) -> None:
        scanner_package = finding(repo_status={"Age": 4})
        baseline_package = finding(repo_status={"Age": 4_000_000})
        result = self.run_checker(
            [scanner_package], 1, [baseline_entry(baseline_package)]
        )

        self.assertEqual(result.returncode, 0)

    def test_new_or_changed_finding_fails_the_ratchet(self) -> None:
        known = finding()
        cases = {
            "new package": finding(name="new-package"),
            "changed version": finding(version="9.9.9"),
            "changed repository status": finding(repo_status="Archived"),
            "changed dependency requirement": finding(
                outdated_deps=[
                    {
                        "name": "dep",
                        "req": ">=2.0",
                        "version_used": "1.0.0",
                        "version_latest": "2.0.0",
                    }
                ]
            ),
        }
        for label, changed in cases.items():
            with self.subTest(label=label):
                result = self.run_checker([changed], 1, [baseline_entry(known)])
                self.assertEqual(result.returncode, 1)
                self.assertIn("unreviewed or changed finding", result.stderr)

    def test_changed_dependency_fields_report_reviewed_and_current_values(self) -> None:
        reviewed_dependency = outdated_dependency()
        reviewed = finding(outdated_deps=[reviewed_dependency])
        changes = {
            "req": ">=1.0",
            "version_used": "1.0.1",
            "version_latest": "2.1.0",
        }
        for field, current_value in changes.items():
            with self.subTest(field=field):
                current = finding(
                    outdated_deps=[{**reviewed_dependency, field: current_value}]
                )
                result = self.run_checker([current], 1, [baseline_entry(reviewed)])

                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    f'outdated_deps["dep"].{field}: '
                    f'reviewed ["{reviewed_dependency[field]}"] '
                    f'-> current ["{current_value}"]',
                    result.stderr,
                )
                current_line = next(
                    line
                    for line in result.stderr.splitlines()
                    if line.startswith("  current identity: ")
                )
                self.assertEqual(
                    json.loads(current_line.removeprefix("  current identity: ")),
                    current,
                )
                reviewed_line = next(
                    line
                    for line in result.stderr.splitlines()
                    if line.startswith("  reviewed identity 1/1: ")
                )
                self.assertEqual(
                    json.loads(
                        reviewed_line.removeprefix("  reviewed identity 1/1: ")
                    ),
                    reviewed,
                )

    def test_changed_repository_status_reports_status_kind_without_age(self) -> None:
        reviewed = finding(repo_status={"Age": 987})
        result = self.run_checker(
            [finding(repo_status="Archived")], 1, [baseline_entry(reviewed)]
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            'repo_status: reviewed "Age" -> current "Archived"', result.stderr
        )
        self.assertIn('"repo_status": "Age"', result.stderr)
        self.assertNotIn("987", result.stderr)

    def test_diagnostics_compare_every_same_package_and_version_candidate(self) -> None:
        reviewed = [
            baseline_entry(finding(repo_status="Unassociated")),
            baseline_entry(finding(repo_status={"Age": 123})),
            baseline_entry(finding(version="9.0.0", repo_status="Nonexistent")),
        ]
        result = self.run_checker([finding(repo_status="Archived")], 1, reviewed)

        self.assertEqual(result.returncode, 1)
        self.assertIn("reviewed identity 1/2:", result.stderr)
        self.assertIn("reviewed identity 2/2:", result.stderr)
        self.assertIn(
            'repo_status: reviewed "Age" -> current "Archived"', result.stderr
        )
        self.assertIn(
            'repo_status: reviewed "Unassociated" -> current "Archived"', result.stderr
        )
        self.assertNotIn('"Nonexistent"', result.stderr)

    def test_new_package_or_version_has_explicit_missing_baseline_diagnostic(self) -> None:
        for current in (finding(name="new-package"), finding(version="9.0.0")):
            with self.subTest(current=current):
                result = self.run_checker([current], 1, [baseline_entry(finding())])

                self.assertEqual(result.returncode, 1)
                self.assertIn(
                    "new package/version: no reviewed baseline identity", result.stderr
                )
                self.assertNotIn("reviewed identity 1/", result.stderr)

    def test_added_and_removed_dependencies_are_reported_without_pairing_names(self) -> None:
        reviewed = finding(outdated_deps=[outdated_dependency()])
        current = finding(outdated_deps=[{**outdated_dependency(), "name": "other"}])
        result = self.run_checker([current], 1, [baseline_entry(reviewed)])

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            'outdated_deps["dep"].req: reviewed ["^1.0"] -> current []', result.stderr
        )
        self.assertIn(
            'outdated_deps["other"].req: reviewed [] -> current ["^1.0"]', result.stderr
        )

    def test_dependency_record_associations_remain_part_of_identity(self) -> None:
        reviewed = finding(
            outdated_deps=[
                outdated_dependency(req="^1", version_used="1.0.0"),
                outdated_dependency(req="^2", version_used="2.0.0"),
            ]
        )
        current = finding(
            outdated_deps=[
                outdated_dependency(req="^1", version_used="2.0.0"),
                outdated_dependency(req="^2", version_used="1.0.0"),
            ]
        )
        result = self.run_checker([current], 1, [baseline_entry(reviewed)])

        self.assertEqual(result.returncode, 1)
        self.assertIn('outdated_deps["dep"] records: reviewed', result.stderr)
        self.assertIn('["dep", "^1", "1.0.0", "2.0.0"]', result.stderr)
        self.assertIn('["dep", "^1", "2.0.0", "2.0.0"]', result.stderr)

    def test_diagnostics_are_independent_of_finding_and_baseline_order(self) -> None:
        current = [finding(name="z"), finding(repo_status="Archived")]
        reviewed = [
            baseline_entry(finding(repo_status="Unassociated")),
            baseline_entry(finding(repo_status={"Age": 987})),
        ]
        forward = self.run_checker(current, 1, reviewed)
        reverse = self.run_checker(list(reversed(current)), 1, list(reversed(reviewed)))

        self.assertEqual(forward.returncode, 1)
        self.assertEqual(reverse.returncode, 1)
        self.assertEqual(forward.stderr, reverse.stderr)

    def test_capture_retains_exact_stdout_once_without_changing_exit_contract(self) -> None:
        cases = [
            ("clean", b" [ ]\r\n", 0, 0, ""),
            ("finding", json.dumps([finding()]).encode(), 1, 1, "unreviewed"),
            ("scanner error", b"[]\n", 7, 2, "unexpected status 7"),
            ("invalid JSON", b"{\r\n", 0, 2, "invalid JSON"),
            ("invalid encoding", b"\xff\r\n", 1, 2, "could not decode"),
            ("invalid schema", b'[{"new_key":true}]\n', 1, 2, "keys"),
            ("contradictory status", b"[]\r\n", 1, 2, "contradicts"),
        ]
        for label, payload, scanner_status, expected_status, diagnostic in cases:
            with self.subTest(label=label):
                baseline = {
                    "schema_version": 1,
                    "repository": "leynos/vtcode",
                    "entries": [],
                }
                with CheckerHarness(payload, scanner_status, baseline) as harness:
                    capture_path = harness.root / "captured.json"
                    baseline_path = (
                        harness.root / "scripts/cargo_unmaintained_baseline.json"
                    )
                    original_baseline = baseline_path.read_bytes()
                    result = harness.run(capture_json=capture_path)

                    self.assertEqual(result.returncode, expected_status)
                    self.assertIn(diagnostic, result.stderr)
                    self.assertEqual(capture_path.read_bytes(), payload)
                    self.assertEqual(harness.invocations_path.read_bytes(), b"scan\n")
                    self.assertEqual(baseline_path.read_bytes(), original_baseline)

    def test_capture_write_failure_fails_closed_after_one_scanner_invocation(self) -> None:
        baseline = {"schema_version": 1, "repository": "leynos/vtcode", "entries": []}
        scanner_results = (("[]", 0), (json.dumps([finding()]), 1), ("{", 7))
        for payload, scanner_status in scanner_results:
            for destination in ("directory", "missing parent"):
                with self.subTest(payload=payload, destination=destination):
                    with CheckerHarness(payload, scanner_status, baseline) as harness:
                        capture_path = (
                            harness.root
                            if destination == "directory"
                            else harness.root / "missing" / "capture.json"
                        )
                        result = harness.run(capture_json=capture_path)

                        self.assertEqual(result.returncode, 2)
                        self.assertIn("cannot capture scanner stdout", result.stderr)
                        self.assertEqual(harness.invocations_path.read_bytes(), b"scan\n")

    def test_capture_precedes_baseline_validation_failure(self) -> None:
        payload = json.dumps([finding()]).encode("utf-8")
        baseline = {
            "schema_version": 1,
            "repository": "leynos/vtcode",
            "entries": [],
            "unexpected": True,
        }
        with CheckerHarness(payload, 1, baseline) as harness:
            capture_path = harness.root / "capture.json"
            result = harness.run(capture_json=capture_path)

            self.assertEqual(result.returncode, 2)
            self.assertIn("baseline keys", result.stderr)
            self.assertEqual(capture_path.read_bytes(), payload)
            self.assertEqual(harness.invocations_path.read_bytes(), b"scan\n")

    def test_missing_current_finding_reports_manual_removal_and_passes(self) -> None:
        result = self.run_checker([], 0, [baseline_entry(finding())])

        self.assertEqual(result.returncode, 0)
        self.assertIn("eligible for manual removal", result.stdout)
        self.assertIn("known-package@1.2.3", result.stdout)

    def test_clean_scan_passes(self) -> None:
        result = self.run_checker([], 0)

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_malformed_scanner_json_fails_closed(self) -> None:
        result = self.run_checker("{", 0)

        self.assertEqual(result.returncode, 2)
        self.assertIn("invalid JSON", result.stderr)

    def test_scanner_schema_is_exact_and_status_age_is_bounded(self) -> None:
        known = finding()
        malformed = [
            {"name": "wrong-top-level"},
            [{**known, "unexpected": True}],
            [finding(repo_status={"Age": -1})],
            [finding(repo_status={"Age": True})],
            [finding(outdated_deps=[{"name": "dep"}])],
        ]
        for payload in malformed:
            with self.subTest(payload=payload):
                result = self.run_checker(payload, 1, [baseline_entry(known)])
                self.assertEqual(result.returncode, 2)

    def test_duplicate_scanner_identity_fails_even_when_age_differs(self) -> None:
        payload = [finding(repo_status={"Age": 1}), finding(repo_status={"Age": 2})]
        result = self.run_checker(payload, 1, [baseline_entry(finding())])

        self.assertEqual(result.returncode, 2)
        self.assertIn("duplicate scanner finding identity", result.stderr)

    def test_malformed_or_duplicate_baseline_fails_closed(self) -> None:
        known = finding()
        baselines = [
            {
                "schema_version": 1,
                "repository": "leynos/vtcode",
                "entries": [baseline_entry(known) | {"unknown": True}],
            },
            {
                "schema_version": 1,
                "repository": "leynos/vtcode",
                "entries": [baseline_entry(known), baseline_entry(known)],
            },
            {
                "schema_version": 1,
                "repository": "other/project",
                "entries": [],
            },
            "not json",
        ]
        for baseline in baselines:
            with self.subTest(baseline=baseline):
                output = json.dumps([known])
                with CheckerHarness(output, 1, baseline) as harness:
                    result = harness.run()
                self.assertEqual(result.returncode, 2)

    def test_non_integer_control_fields_fail_closed_with_field_diagnostics(
        self,
    ) -> None:
        known = finding()
        valid_baseline = {
            "schema_version": 1,
            "repository": "leynos/vtcode",
            "entries": [baseline_entry(known)],
        }
        cases = [
            (
                "age boolean",
                [finding(repo_status={"Age": True})],
                valid_baseline,
                "scanner.findings[0].repo_status.Age must be a non-negative integer",
            ),
            (
                "age float",
                [finding(repo_status={"Age": 1.5})],
                valid_baseline,
                "scanner.findings[0].repo_status.Age must be a non-negative integer",
            ),
            (
                "schema version boolean",
                [known],
                {**valid_baseline, "schema_version": True},
                "baseline.schema_version must be 1",
            ),
            (
                "schema version float",
                [known],
                {**valid_baseline, "schema_version": 1.0},
                "baseline.schema_version must be 1",
            ),
            (
                "issue boolean",
                [known],
                {
                    **valid_baseline,
                    "entries": [baseline_entry(known) | {"issue": True}],
                },
                "baseline.entries[0].issue must be 108",
            ),
            (
                "issue float",
                [known],
                {
                    **valid_baseline,
                    "entries": [baseline_entry(known) | {"issue": 108.0}],
                },
                "baseline.entries[0].issue must be 108",
            ),
        ]
        for label, scanner_output, baseline, diagnostic in cases:
            with self.subTest(label=label):
                with CheckerHarness(json.dumps(scanner_output), 1, baseline) as harness:
                    result = harness.run()

                self.assertEqual(result.returncode, 2)
                self.assertIn(diagnostic, result.stderr)

    def test_scanner_operational_failure_fails_even_with_plausible_json(self) -> None:
        for scanner_status in (2, 7):
            with self.subTest(scanner_status=scanner_status):
                result = self.run_checker(
                    [finding()], scanner_status, [baseline_entry(finding())]
                )

                self.assertEqual(result.returncode, 2)
                self.assertIn(f"unexpected status {scanner_status}", result.stderr)

    def test_contradictory_scanner_status_and_output_fail_closed(self) -> None:
        cases = [(1, []), (0, [finding()])]
        for scanner_status, output in cases:
            with self.subTest(scanner_status=scanner_status, output=output):
                result = self.run_checker(
                    output, scanner_status, [baseline_entry(finding())]
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("contradicts finding count", result.stderr)

    def test_scanner_launch_failure_fails_closed(self) -> None:
        baseline = {
            "schema_version": 1,
            "repository": "leynos/vtcode",
            "entries": [],
        }
        with CheckerHarness("[]", 0, baseline) as harness:
            result = subprocess.run(
                [
                    sys.executable,
                    str(harness.checker),
                    "--scanner",
                    str(harness.root / "missing-scanner"),
                ],
                cwd=harness.root,
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertEqual(result.returncode, 2)
        self.assertIn("could not launch scanner", result.stderr)

    def test_literal_scanner_path_and_arguments_are_not_shell_interpreted(self) -> None:
        marker_name = self._marker_path()
        scanner_name = f"scanner; touch {marker_name}"
        package = finding()
        baseline = baseline_entry(package)
        output = json.dumps([package])
        baseline_document = {
            "schema_version": 1,
            "repository": "leynos/vtcode",
            "entries": [baseline],
        }
        with CheckerHarness(output, 1, baseline_document) as harness:
            arguments_path = harness.root / "arguments.json"
            scanner = harness.root / scanner_name
            scanner.write_text(
                "#!/usr/bin/env python3\n"
                "import json\n"
                "import pathlib\n"
                "import sys\n"
                f"pathlib.Path({str(arguments_path)!r}).write_text(json.dumps(sys.argv[1:]))\n"
                f"sys.stdout.write({output!r})\n"
                "raise SystemExit(1)\n",
                encoding="utf-8",
            )
            scanner.chmod(scanner.stat().st_mode | stat.S_IXUSR)
            result = subprocess.run(
                [sys.executable, str(harness.checker), "--scanner", str(scanner)],
                cwd=harness.root,
                capture_output=True,
                text=True,
                check=False,
            )
            arguments = json.loads(arguments_path.read_text(encoding="utf-8"))
            marker = harness.root / marker_name

        self.assertEqual(result.returncode, 0)
        self.assertEqual(arguments, ["unmaintained", "--json", "--no-warnings"])
        self.assertFalse(marker.exists())

    @staticmethod
    def _marker_path() -> str:
        return "created-by-shell-interpretation"


if __name__ == "__main__":
    unittest.main()
