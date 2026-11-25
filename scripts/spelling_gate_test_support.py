"""Controlled-process fixtures for the tracked-file spelling gate."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import textwrap
from collections.abc import Mapping
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHECKER = REPOSITORY_ROOT / "scripts" / "check-spelling.sh"
MAKEFILE = REPOSITORY_ROOT / "Makefile"
POLICY = REPOSITORY_ROOT / "typos.toml"


def _write_executable(path: Path, source: str) -> Path:
    """Write a Python fixture with a valid shebang and executable mode."""
    path.write_text(source.replace("__PYTHON__", sys.executable), encoding="utf-8")
    path.chmod(0o755)
    return path


def write_fake_git(directory: Path) -> tuple[Path, Path, Path]:
    """Write a Git fixture controlled by NUL output and an exit status.

    The fixture records every invocation as one JSON line, then either writes
    the prepared NUL-separated output or exits with ``FAKE_GIT_STATUS``.

    Parameters
    ----------
    directory : Path
        Existing directory that receives the executable; it also holds the
        output file and call log returned to the caller.

    Returns
    -------
    tuple[Path, Path, Path]
        The executable, the file holding the NUL-separated ``ls-files``
        output, and the JSON-lines invocation log.

    Raises
    ------
    OSError
        If the fixture files cannot be written into ``directory``.
    """
    executable = directory / "git"
    output = directory / "git-output.bin"
    call_log = directory / "git-calls.jsonl"
    _write_executable(
        executable,
        textwrap.dedent(
            """\
            #!__PYTHON__
            import json
            import os
            from pathlib import Path
            import sys

            with Path(os.environ["FAKE_GIT_CALL_LOG"]).open("a", encoding="utf-8") as log:
                print(json.dumps(sys.argv[1:]), file=log)
            status = int(os.environ.get("FAKE_GIT_STATUS", "0"))
            if status:
                print("controlled git failure", file=sys.stderr)
                raise SystemExit(status)
            sys.stdout.buffer.write(Path(os.environ["FAKE_GIT_OUTPUT_FILE"]).read_bytes())
            """
        ),
    )
    return executable, output, call_log


def write_fake_typos(directory: Path) -> tuple[Path, Path]:
    """Write a typos fixture that records one exact invocation.

    The fixture logs its arguments as one JSON line and exits with
    ``FAKE_TYPOS_STATUS``.

    Parameters
    ----------
    directory : Path
        Existing directory that receives the executable and the returned call
        log path.

    Returns
    -------
    tuple[Path, Path]
        The executable and the JSON-lines invocation log.

    Raises
    ------
    OSError
        If the fixture files cannot be written into ``directory``.
    """
    executable = directory / "typos"
    call_log = directory / "typos-calls.jsonl"
    _write_executable(
        executable,
        textwrap.dedent(
            """\
            #!__PYTHON__
            import json
            import os
            from pathlib import Path
            import sys

            with Path(os.environ["FAKE_TYPOS_CALL_LOG"]).open("a", encoding="utf-8") as log:
                print(json.dumps(sys.argv[1:]), file=log)
            raise SystemExit(int(os.environ.get("FAKE_TYPOS_STATUS", "0")))
            """
        ),
    )
    return executable, call_log


def prepare_make_repository(directory: Path) -> None:
    """Copy only spelling-gate inputs into a controlled process repository.

    Parameters
    ----------
    directory : Path
        Existing directory that receives ``Makefile``, ``typos.toml`` and the
        gate script under ``scripts/``.

    Raises
    ------
    FileExistsError
        If ``directory/scripts`` already exists.
    OSError
        If ``directory`` is missing or an input cannot be copied.
    """
    scripts = directory / "scripts"
    scripts.mkdir()
    shutil.copy2(MAKEFILE, directory / MAKEFILE.name)
    shutil.copy2(CHECKER, scripts / CHECKER.name)
    shutil.copy2(POLICY, directory / POLICY.name)


def run_process(
    arguments: list[str],
    *,
    cwd: Path,
    environment: Mapping[str, str],
) -> subprocess.CompletedProcess[str]:
    """Run one controlled process and retain its diagnostics.

    Parameters
    ----------
    arguments : list[str]
        Program and arguments to execute.
    cwd : Path
        Working directory for the child process.
    environment : Mapping[str, str]
        Variables overlaid on the inherited environment.

    Returns
    -------
    subprocess.CompletedProcess[str]
        Completed process whose non-zero status is reported instead of
        raised.

    Raises
    ------
    OSError
        If the program cannot be started.
    """
    return subprocess.run(
        arguments,
        check=False,
        capture_output=True,
        cwd=cwd,
        env=os.environ | dict(environment),
        text=True,
    )


def read_json_lines(path: Path) -> list[list[str]]:
    """Return JSON invocation records, treating no invocation as an empty list.

    Parameters
    ----------
    path : Path
        JSON-lines call log; a missing file means no invocation was recorded.

    Returns
    -------
    list[list[str]]
        One decoded argument list per recorded invocation, in file order.

    Raises
    ------
    OSError
        If an existing log cannot be read.
    json.JSONDecodeError
        If a line is not a complete JSON value.
    """
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
