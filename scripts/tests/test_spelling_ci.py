"""Keep the typos CI job pinned to the local spelling gate."""

from __future__ import annotations

from pathlib import Path
import unittest


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
CI_WORKFLOW = REPOSITORY_ROOT / ".github" / "workflows" / "ci.yml"
INSTALL_ACTION = "taiki-e/install-action@fae525311e5e299134b606bf247d465dd0df8190"


class SpellingCiContractTests(unittest.TestCase):
    """Assert every spelling-dependent CI job installs the pinned tool first."""

    def assert_installs_typos_before(self, job: str, command: str) -> None:
        """Require the shared pinned installation before the Make invocation."""
        self.assertIn(INSTALL_ACTION, job)
        self.assertIn("tool: typos-cli@1.50.1", job)
        self.assertIn(command, job)
        self.assertLess(job.index(INSTALL_ACTION), job.index(command))

    def test_spelling_dependent_jobs_install_typos_before_make(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        typos_job = workflow.split("  lint-typos:\n", 1)[1].split(
            "\n  # Check for any", 1
        )[0]
        clippy_job = workflow.split("  lint-clippy:\n", 1)[1].split(
            "\n  # Check for unmaintained", 1
        )[0]

        self.assertIn("name: Check Typos", typos_job)
        self.assertIn("timeout-minutes: 10", typos_job)
        self.assertIn("contents: read", typos_job)
        self.assertIn(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
            typos_job,
        )
        self.assert_installs_typos_before(typos_job, "run: /usr/bin/make spelling")
        self.assertNotIn("crate-ci/typos@master", typos_job)
        self.assert_installs_typos_before(
            clippy_job,
            'run: /usr/bin/make BUILD_JOBS="--jobs 4" ACTIONLINT="$GITHUB_WORKSPACE/actionlint" lint',
        )


if __name__ == "__main__":
    unittest.main()
