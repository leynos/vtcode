"""Small parsed-YAML and controlled-process helpers for workflow validation."""

from __future__ import annotations

import os
import re
from pathlib import Path
import subprocess
import sys
from typing import Mapping

import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIRECTORY = REPOSITORY_ROOT / ".github" / "workflows"
CI_WORKFLOW_PATH = WORKFLOW_DIRECTORY / "ci.yml"
MAKEFILE_PATH = REPOSITORY_ROOT / "Makefile"
YAMLLINT_POLICY_PATH = REPOSITORY_ROOT / ".yamllint.yml"


class WorkflowLoader(yaml.SafeLoader):
    """Resolve booleans using YAML 1.2 semantics for GitHub's ``on`` key."""


WorkflowLoader.yaml_implicit_resolvers = {
    initial: [
        (tag, expression)
        for tag, expression in resolvers
        if tag != "tag:yaml.org,2002:bool"
    ]
    for initial, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()
}
WorkflowLoader.add_implicit_resolver(
    "tag:yaml.org,2002:bool",
    re.compile(r"^(?:true|True|TRUE|false|False|FALSE)$"),
    list("tTfF"),
)


def load_workflow(path: Path) -> dict[str, object]:
    """Parse one workflow and require a string-keyed mapping root."""
    loader = WorkflowLoader(path.read_text(encoding="utf-8"))
    try:
        document = loader.get_single_data()
    finally:
        loader.dispose()
    if not isinstance(document, dict):
        raise AssertionError(f"{path} must parse to a mapping")
    if not all(isinstance(key, str) for key in document):
        raise AssertionError(f"{path} must use string keys")
    return document


def job_steps(workflow: dict[str, object], name: str) -> list[dict[str, object]]:
    """Return the named job's mapping steps in declared order."""
    jobs = workflow.get("jobs")
    if not isinstance(jobs, dict):
        raise AssertionError("workflow jobs must be a mapping")
    job = jobs.get(name)
    if not isinstance(job, dict):
        raise AssertionError(f"jobs.{name} must be a mapping")
    steps = job.get("steps")
    if not isinstance(steps, list) or not all(isinstance(step, dict) for step in steps):
        raise AssertionError(f"jobs.{name}.steps must be a list of mappings")
    return steps


def named_step(steps: list[dict[str, object]], name: str) -> dict[str, object]:
    """Return the unique step with ``name``."""
    matches = [step for step in steps if step.get("name") == name]
    if len(matches) != 1:
        raise AssertionError(f"expected one {name!r} step, found {len(matches)}")
    return matches[0]


def run_process(
    arguments: list[str],
    *,
    cwd: Path = REPOSITORY_ROOT,
    environment: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a controlled subprocess and capture its diagnostics."""
    return subprocess.run(
        arguments,
        check=False,
        capture_output=True,
        cwd=cwd,
        env=os.environ | dict(environment or {}),
        text=True,
    )


ACTIONLINT_SHA256 = "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"
ACTIONLINT_INSTALLER_URL = (
    "https://raw.githubusercontent.com/rhysd/actionlint/"
    "914e7df21a07ef503a81201c76d2b11c789d3fca/scripts/download-actionlint.bash"
)
ACTIONLINT_RELEASE_URL = (
    "https://github.com/rhysd/actionlint/releases/download/"
    "v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz"
)
VERIFIED_ACTIONLINT_ARCHIVE = b"verified actionlint archive"


def _write_executable(path: Path, lines: list[str]) -> None:
    """Write an executable script with a first-byte shebang."""
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    path.chmod(0o755)


def write_fake_linter(directory: Path, name: str) -> Path:
    """Write a linter fixture that logs its name and returns a selected status."""
    executable = directory / name
    argument_key = f"FAKE_{name.upper()}_ARGS"
    status_key = f"FAKE_{name.upper()}_STATUS"
    _write_executable(
        executable,
        [
            f"#!{sys.executable}",
            "import json",
            "import os",
            "from pathlib import Path",
            "import sys",
            "",
            f"expected = json.loads(os.environ[{argument_key!r}])",
            "if sys.argv[1:] != expected:",
            "    print(",
            f"        {('unexpected ' + name + ' arguments: ')!r} + repr(sys.argv[1:]),",
            "        file=sys.stderr,",
            "    )",
            "    raise SystemExit(64)",
            'with Path(os.environ["FAKE_LINTER_CALL_LOG"]).open("a", encoding="utf-8") as log:',
            f"    print({name!r}, file=log)",
            f"raise SystemExit(int(os.environ[{status_key!r}]))",
        ],
    )
    return executable


def write_actionlint_download_fixtures(
    directory: Path,
    *,
    checksum_status: int = 0,
    installer_download_status: int = 0,
) -> tuple[Path, Path, Path, Path]:
    """Write strict fixtures for the verified actionlint installer boundary."""
    curl = directory / "curl"
    sha256sum = directory / "sha256sum"
    call_log = directory / "curl-calls.log"
    installer_log = directory / "installer-calls.log"
    installer_contents = (
        'printf "installer-ran\n" >> "${ACTIONLINT_INSTALLER_CALL_LOG}"\n'
        'curl "${ACTIONLINT_RELEASE_URL}" > "${ACTIONLINT_OUTPUT_PATH}"\n'
    )
    _write_executable(
        curl,
        [
            f"#!{sys.executable}",
            "import os",
            "from pathlib import Path",
            "import sys",
            "",
            "arguments = sys.argv[1:]",
            "expected_prefix = ['--fail', '--location', '--show-error', '--output']",
            "if arguments[:4] != expected_prefix or len(arguments) != 6:",
            "    print('unexpected curl flags: ' + repr(arguments), file=sys.stderr)",
            "    raise SystemExit(96)",
            "destination = arguments[4]",
            "url = arguments[5]",
            'with Path(os.environ["ACTIONLINT_CURL_CALL_LOG"]).open("a", encoding="utf-8") as log:',
            "    print(url, file=log)",
            f"if url == {ACTIONLINT_INSTALLER_URL!r}:",
            f"    if {installer_download_status}:",
            f"        raise SystemExit({installer_download_status})",
            f"    Path(destination).write_text({installer_contents!r}, encoding='utf-8')",
            f"elif url == {ACTIONLINT_RELEASE_URL!r}:",
            "    Path(destination).write_bytes(",
            f"        bytes.fromhex({VERIFIED_ACTIONLINT_ARCHIVE.hex()!r})",
            "    )",
            "else:",
            "    print('unexpected curl request: ' + repr(arguments), file=sys.stderr)",
            "    raise SystemExit(97)",
        ],
    )
    _write_executable(
        sha256sum,
        [
            f"#!{sys.executable}",
            "import sys",
            "",
            "if sys.argv[1:] != ['--check', '--']:",
            "    print('unexpected sha256sum flags: ' + repr(sys.argv[1:]), file=sys.stderr)",
            "    raise SystemExit(96)",
            "manifest = sys.stdin.read()",
            f"expected_prefix = {ACTIONLINT_SHA256!r} + '  '",
            "if not manifest.startswith(expected_prefix) or not manifest.endswith('\\n'):",
            "    print('unexpected checksum manifest: ' + repr(manifest), file=sys.stderr)",
            "    raise SystemExit(97)",
            "archive_path = manifest[len(expected_prefix):-1]",
            "if not archive_path or '\\n' in archive_path:",
            "    print('unexpected checksum archive path: ' + repr(archive_path), file=sys.stderr)",
            "    raise SystemExit(98)",
            "with open(archive_path, 'rb') as archive:",
            f"    if archive.read() != bytes.fromhex({VERIFIED_ACTIONLINT_ARCHIVE.hex()!r}):",
            "        print('checksum fixture received unexpected archive', file=sys.stderr)",
            "        raise SystemExit(99)",
            f"raise SystemExit({checksum_status})",
        ],
    )
    return curl, sha256sum, call_log, installer_log


def read_call_log(path: Path) -> list[str]:
    """Read fixture invocation names in order."""
    if not path.exists():
        return []
    return path.read_text(encoding="utf-8").splitlines()
