"""Keep the typos CI job pinned to the local spelling gate."""

from __future__ import annotations

import unittest
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_DIRECTORY = REPOSITORY_ROOT / ".github" / "workflows"
SPELLING_WORKFLOW = WORKFLOW_DIRECTORY / "spelling.yml"
CI_WORKFLOW = WORKFLOW_DIRECTORY / "ci.yml"
INSTALL_ACTION = "taiki-e/install-action@fae525311e5e299134b606bf247d465dd0df8190"


class SpellingCiContractTests(unittest.TestCase):
    """Assert every spelling-dependent CI job installs the pinned tool first."""

    def assert_installs_typos_before(self, job: str, command: str) -> None:
        """Require the shared pinned installation before the Make invocation."""
        self.assertIn(INSTALL_ACTION, job)
        self.assertIn("tool: typos-cli@1.50.1", job)
        self.assertIn(command, job)
        self.assertLess(job.index(INSTALL_ACTION), job.index(command))

    def test_spelling_workflow_runs_for_every_tracked_file(self) -> None:
        """Reject path filters that would skip tracked file types."""
        workflow = SPELLING_WORKFLOW.read_text(encoding="utf-8")
        triggers = workflow.split("\non:\n", 1)[1].split("\ndefaults:\n", 1)[0]
        self.assertNotIn("paths:", triggers)

    def test_spelling_check_installs_typos_before_make(self) -> None:
        workflow = SPELLING_WORKFLOW.read_text(encoding="utf-8")
        typos_job = workflow.split("  lint-typos:\n", 1)[1]

        self.assertIn("name: Check Typos", typos_job)
        self.assertIn("timeout-minutes: 10", typos_job)
        self.assertIn("contents: read", typos_job)
        self.assertIn(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
            typos_job,
        )
        self.assert_installs_typos_before(typos_job, "run: /usr/bin/make spelling")
        self.assertNotIn("crate-ci/typos@master", typos_job)

    def test_clippy_job_installs_typos_before_make_lint(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        clippy_job = workflow.split("  lint-clippy:\n", 1)[1].split(
            "\n  # Check for unmaintained", 1
        )[0]

        self.assert_installs_typos_before(
            clippy_job,
            'run: /usr/bin/make BUILD_JOBS="--jobs 4" '
            'ACTIONLINT="$GITHUB_WORKSPACE/actionlint" lint',
        )


if __name__ == "__main__":
    unittest.main()