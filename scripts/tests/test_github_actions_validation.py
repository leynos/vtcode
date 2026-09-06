"""Exercise GitHub Actions linting through parsed YAML and fake linters."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import unittest

from workflow_validation_test_support import (
    CI_WORKFLOW_PATH,
    MAKEFILE_PATH,
    WORKFLOW_DIRECTORY,
    YAMLLINT_POLICY_PATH,
    load_workflow,
    read_call_log,
    run_process,
    write_actionlint_download_fixtures,
    write_fake_linter,
)


class WorkflowPolicyTests(unittest.TestCase):
    """Keep the repository YAML policy and workflow documents aligned."""

    def test_policy_keeps_github_actions_compatible_defaults(self) -> None:
        policy = YAMLLINT_POLICY_PATH.read_text(encoding="utf-8")
        for required in (
            "extends: default",
            "present: true",
            "max: 120",
            "allowed-values: ['true', 'false']",
            "check-keys: false",
        ):
            with self.subTest(required=required):
                self.assertIn(required, policy)

    def test_every_workflow_starts_and_parses_as_a_mapping(self) -> None:
        workflow_paths = sorted(
            {*WORKFLOW_DIRECTORY.glob("*.yml"), *WORKFLOW_DIRECTORY.glob("*.yaml")}
        )
        self.assertTrue(workflow_paths)
        for path in workflow_paths:
            with self.subTest(path=path.name):
                source = path.read_text(encoding="utf-8")
                self.assertTrue(source.startswith("---\n"))
                self.assertEqual(source.count("\n---\n") + 1, 1)
                self.assertFalse(source.endswith("\n...\n"))
                self.assertTrue(
                    all(len(line) <= 120 for line in source.splitlines()),
                    "yamllint line-length policy requires every workflow line to fit 120 columns",
                )
                workflow = load_workflow(path)
                self.assertIn("on", workflow)
                self.assertNotIn(True, workflow)

    def test_makefile_changes_trigger_push_and_pull_request(self) -> None:
        """Run workflow validation when its Makefile integration changes."""
        workflow = load_workflow(CI_WORKFLOW_PATH)
        triggers = workflow.get("on")
        self.assertIsInstance(triggers, dict)
        assert isinstance(triggers, dict)
        for event in ("push", "pull_request"):
            with self.subTest(event=event):
                filter_mapping = triggers.get(event)
                self.assertIsInstance(filter_mapping, dict)
                assert isinstance(filter_mapping, dict)
                self.assertEqual(filter_mapping.get("branches"), ["main"])
                paths = filter_mapping.get("paths")
                self.assertIsInstance(paths, list)
                assert isinstance(paths, list)
                self.assertIn("Makefile", paths)

    def test_makefile_wires_policy_before_actionlint(self) -> None:
        makefile = MAKEFILE_PATH.read_text(encoding="utf-8")
        self.assertIn(
            "lint: lint-shell lint-policies github-actions-lint lint-clippy lint-docs",
            makefile,
        )
        target = "github-actions-lint:\n"
        start = makefile.index(target) + len(target)
        recipe = makefile[start:].split("\n\n", 1)[0].splitlines()
        self.assertEqual(
            recipe,
            [
                "\t$(YAMLLINT) --config-file .yamllint.yml .github/workflows",
                "\t$(ACTIONLINT)",
            ],
        )


class MakefileProcessTests(unittest.TestCase):
    """Verify the policy-first Make target without invoking third-party linters."""

    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary_directory.name)
        self.call_log = self.directory / "calls.log"
        self.yamllint = write_fake_linter(self.directory, "yamllint")
        self.actionlint = write_fake_linter(self.directory, "actionlint")

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def run_target(self, yamllint_status: int, actionlint_status: int):
        self.call_log.unlink(missing_ok=True)
        environment = {
            "FAKE_LINTER_CALL_LOG": str(self.call_log),
            "FAKE_YAMLLINT_ARGS": json.dumps(
                ["--config-file", ".yamllint.yml", ".github/workflows"]
            ),
            "FAKE_ACTIONLINT_ARGS": "[]",
            "FAKE_YAMLLINT_STATUS": str(yamllint_status),
            "FAKE_ACTIONLINT_STATUS": str(actionlint_status),
            "PATH": f"{self.directory}{os.pathsep}{os.environ['PATH']}",
        }
        return run_process(
            [
                "make",
                "--no-print-directory",
                "github-actions-lint",
                f"YAMLLINT={self.yamllint}",
                f"ACTIONLINT={self.actionlint}",
            ],
            environment=environment,
        )

    def test_success_runs_linters_in_policy_first_order(self) -> None:
        result = self.run_target(0, 0)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_call_log(self.call_log), ["yamllint", "actionlint"])

    def test_every_yamllint_exit_status_stops_actionlint(self) -> None:
        for status in range(256):
            with self.subTest(status=status):
                result = self.run_target(status, 0)
                expected = 0 if status == 0 else 2
                self.assertEqual(result.returncode, expected, result.stderr)
                expected_calls = ["yamllint"]
                if status == 0:
                    expected_calls.append("actionlint")
                self.assertEqual(read_call_log(self.call_log), expected_calls)

    def test_every_actionlint_exit_status_propagates_after_yamllint(self) -> None:
        for status in range(256):
            with self.subTest(status=status):
                result = self.run_target(0, status)
                expected = 0 if status == 0 else 2
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(read_call_log(self.call_log), ["yamllint", "actionlint"])

    def actionlint_download_script(self) -> str:
        """Return the parsed verified-installer script from the CI workflow."""
        workflow = load_workflow(CI_WORKFLOW_PATH)
        jobs = workflow["jobs"]
        self.assertIsInstance(jobs, dict)
        assert isinstance(jobs, dict)
        job = jobs["lint-clippy"]
        self.assertIsInstance(job, dict)
        assert isinstance(job, dict)
        steps = job["steps"]
        self.assertIsInstance(steps, list)
        assert isinstance(steps, list)
        download = next(
            step for step in steps if step.get("name") == "Download actionlint"
        )
        script = download.get("run")
        self.assertIsInstance(script, str)
        assert isinstance(script, str)
        return script

    def run_actionlint_download(
        self,
        directory: Path,
        *,
        checksum_status: int = 0,
        installer_download_status: int = 0,
    ):
        """Execute the CI script with strict controlled download fixtures."""
        _curl, _sha256sum, call_log, installer_log = write_actionlint_download_fixtures(
            directory,
            checksum_status=checksum_status,
            installer_download_status=installer_download_status,
        )
        output = directory / "installed-actionlint"
        result = run_process(
            ["bash", "-c", self.actionlint_download_script()],
            environment={
                "ACTIONLINT_CURL_CALL_LOG": str(call_log),
                "ACTIONLINT_INSTALLER_CALL_LOG": str(installer_log),
                "ACTIONLINT_OUTPUT_PATH": str(output),
                "PATH": f"{directory}{os.pathsep}{os.environ['PATH']}",
            },
        )
        return result, output, call_log, installer_log

    def test_installer_child_receives_the_verified_archive_without_a_second_fetch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result, output, call_log, installer_log = self.run_actionlint_download(
                Path(temporary)
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_bytes(), b"verified actionlint archive")
            self.assertEqual(read_call_log(installer_log), ["installer-ran"])
            self.assertEqual(
                read_call_log(call_log),
                [
                    "https://raw.githubusercontent.com/rhysd/actionlint/"
                    "914e7df21a07ef503a81201c76d2b11c789d3fca/"
                    "scripts/download-actionlint.bash",
                    "https://github.com/rhysd/actionlint/releases/download/"
                    "v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz",
                ],
            )

    def test_checksum_failure_does_not_run_the_installer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result, output, call_log, installer_log = self.run_actionlint_download(
                Path(temporary), checksum_status=23
            )

            self.assertEqual(result.returncode, 23, result.stderr)
            self.assertFalse(output.exists())
            self.assertFalse(installer_log.exists())
            self.assertEqual(len(read_call_log(call_log)), 2)

    def test_installer_download_failure_is_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result, output, call_log, installer_log = self.run_actionlint_download(
                Path(temporary), installer_download_status=24
            )

            self.assertEqual(result.returncode, 24, result.stderr)
            self.assertFalse(output.exists())
            self.assertFalse(installer_log.exists())
            self.assertEqual(len(read_call_log(call_log)), 1)



if __name__ == "__main__":
    unittest.main()
