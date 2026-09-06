"""Hold the CI workflow to its pinned GitHub Actions validation contract."""

from __future__ import annotations

import unittest

from workflow_validation_test_support import (
    CI_WORKFLOW_PATH,
    job_steps,
    load_workflow,
    named_step,
)

CACHE_ACTION = "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
SETUP_UV_ACTION = "astral-sh/setup-uv@11f9893b081a58869d3b5fccaea48c9e9e46f990"
ACTIONLINT_VERSION = "1.7.12"
ACTIONLINT_SHA256 = "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"
ACTIONLINT_INSTALLER_COMMIT = "914e7df21a07ef503a81201c76d2b11c789d3fca"


class WorkflowValidationCiTests(unittest.TestCase):
    """Assert provisioning, hardening, and trusted lint invocation as parsed YAML."""

    def setUp(self) -> None:
        self.workflow = load_workflow(CI_WORKFLOW_PATH)
        self.steps = job_steps(self.workflow, "lint-clippy")
        jobs = self.workflow["jobs"]
        assert isinstance(jobs, dict)
        self.job = jobs["lint-clippy"]
        assert isinstance(self.job, dict)

    def test_pinned_yamllint_provisioning_precedes_actionlint(self) -> None:
        environment = self.job.get("env")
        self.assertIsInstance(environment, dict)
        assert isinstance(environment, dict)
        self.assertEqual(environment.get("YAMLLINT_VERSION"), "1.38.0")
        self.assertEqual(environment.get("UV_CACHE_DIR"), "${{ github.workspace }}/.uv-cache")
        self.assertEqual(environment.get("UV_TOOL_DIR"), "${{ github.workspace }}/.uv-tools")
        self.assertEqual(environment.get("UV_TOOL_BIN_DIR"), "${{ github.workspace }}/.uv-bin")

        names = [step.get("name") for step in self.steps]
        expected = [
            "Setup uv",
            "Cache yamllint",
            "Install yamllint",
            "Cache actionlint",
            "Download actionlint",
            "Lint",
        ]
        positions = [names.index(name) for name in expected]
        self.assertEqual(positions, sorted(positions))

        setup_uv = named_step(self.steps, "Setup uv")
        self.assertEqual(setup_uv.get("uses"), SETUP_UV_ACTION)
        self.assertEqual(setup_uv.get("with"), {"enable-cache": "false"})

        cache_yamllint = named_step(self.steps, "Cache yamllint")
        self.assertEqual(cache_yamllint.get("uses"), CACHE_ACTION)
        self.assertEqual(
            cache_yamllint.get("with"),
            {
                "path": ".uv-cache\n.uv-tools\n.uv-bin\n",
                "key": "yamllint-${{ runner.os }}-${{ runner.arch }}-${{ env.YAMLLINT_VERSION }}",
            },
        )
        install_yamllint = named_step(self.steps, "Install yamllint")
        self.assertEqual(
            install_yamllint.get("run"),
            'uv tool install "yamllint==${YAMLLINT_VERSION}"\n'
            'echo "${UV_TOOL_BIN_DIR}" >> "$GITHUB_PATH"\n',
        )

    def test_verified_actionlint_download_precedes_installer(self) -> None:
        cache = named_step(self.steps, "Cache actionlint")
        self.assertEqual(cache.get("id"), "cache_actionlint")
        self.assertEqual(cache.get("uses"), CACHE_ACTION)
        self.assertEqual(
            cache.get("with"),
            {
                "path": "actionlint",
                "key": "actionlint-${{ runner.os }}-${{ runner.arch }}-1.7.12",
            },
        )

        download = named_step(self.steps, "Download actionlint")
        self.assertEqual(download.get("if"), "steps.cache_actionlint.outputs.cache-hit != 'true'")
        self.assertEqual(download.get("shell"), "bash")
        script = download.get("run")
        self.assertIsInstance(script, str)
        assert isinstance(script, str)
        for fragment in (
            f"readonly ACTIONLINT_VERSION='{ACTIONLINT_VERSION}'",
            f"readonly ACTIONLINT_SHA256='{ACTIONLINT_SHA256}'",
            f"readonly ACTIONLINT_INSTALLER_COMMIT='{ACTIONLINT_INSTALLER_COMMIT}'",
            'readonly ACTIONLINT_ARCHIVE="actionlint_${ACTIONLINT_VERSION}_linux_amd64.tar.gz"',
            "readonly ACTIONLINT_RAW_BASE='https://raw.githubusercontent.com/rhysd/actionlint'",
            "readonly ACTIONLINT_SCRIPT='scripts/download-actionlint.bash'",
            'readonly ACTIONLINT_INSTALLER_URL="${ACTIONLINT_RAW_BASE}/${ACTIONLINT_INSTALLER_COMMIT}/${ACTIONLINT_SCRIPT}"',
            "readonly ACTIONLINT_RELEASE_ROOT='https://github.com/rhysd/actionlint/releases/download'",
            'readonly ACTIONLINT_RELEASE_BASE="${ACTIONLINT_RELEASE_ROOT}/v${ACTIONLINT_VERSION}"',
            'readonly ACTIONLINT_RELEASE_URL="${ACTIONLINT_RELEASE_BASE}/${ACTIONLINT_ARCHIVE}"',
            "command curl --fail --location --show-error --output",
            "printf '%s  %s\\n' \"${ACTIONLINT_SHA256}\" \"${ACTIONLINT_ARCHIVE_PATH}\" | sha256sum --check --",
            "curl() {",
            'cat "${ACTIONLINT_ARCHIVE_PATH}"',
            'export ACTIONLINT_RELEASE_URL ACTIONLINT_ARCHIVE_PATH',
            'bash "${ACTIONLINT_INSTALLER_PATH}" "${ACTIONLINT_VERSION}"',
        ):
            with self.subTest(fragment=fragment):
                self.assertIn(fragment, script)
        self.assertLess(
            script.index("sha256sum --check --"),
            script.index('bash "${ACTIONLINT_INSTALLER_PATH}"'),
        )

    def test_ci_uses_the_trusted_make_binary_and_cached_actionlint(self) -> None:
        lint = named_step(self.steps, "Lint")
        self.assertEqual(
            lint.get("run"),
            '/usr/bin/make BUILD_JOBS="--jobs 4" ACTIONLINT="$GITHUB_WORKSPACE/actionlint" lint',
        )
        contract = named_step(self.steps, "Validate GitHub Actions lint contract")
        self.assertEqual(contract.get("run"), "/usr/bin/make test-github-actions-validation")


if __name__ == "__main__":
    unittest.main()
