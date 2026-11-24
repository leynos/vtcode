"""Exercise the spelling gate's NUL-safe tracked-file discovery."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from spelling_gate_test_support import (
    prepare_make_repository,
    read_json_lines,
    run_process,
    write_fake_git,
    write_fake_typos,
)


class SpellingGateProcessTests(unittest.TestCase):
    """Keep the spelling gate scoped to existing, tracked repository paths.

    Each test runs the real ``Makefile``, checker and policy in a temporary
    repository whose Git and typos executables are supplied by the test.
    """

    def setUp(self) -> None:
        """Create an isolated repository and its controlled executables.

        The temporary directory receives the copied gate inputs and the
        fixture executables; the environment passed to ``run_target`` points
        at the fixture logs and prepared output file.
        """
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary_directory.name)
        prepare_make_repository(self.directory)
        fixtures = self.directory / "fixtures"
        fixtures.mkdir()
        self.git, self.git_output, self.git_log = write_fake_git(fixtures)
        self.typos, self.typos_log = write_fake_typos(fixtures)
        self.environment = {
            "FAKE_GIT_CALL_LOG": str(self.git_log),
            "FAKE_GIT_OUTPUT_FILE": str(self.git_output),
            "FAKE_TYPOS_CALL_LOG": str(self.typos_log),
        }

    def tearDown(self) -> None:
        """Remove the temporary repository created by ``setUp``."""
        self.temporary_directory.cleanup()

    def write_tracked_paths(self, *paths: str) -> None:
        """Write the NUL-separated tracked paths reported by the fake Git.

        Parameters
        ----------
        *paths : str
            Repository-relative paths in the order the gate must pass them to
            typos.
        """
        self.git_output.write_bytes(b"".join(path.encode() + b"\0" for path in paths))

    def run_target(self, *, typos: Path | None = None):
        """Run the make spelling target with the controlled fixtures.

        Parameters
        ----------
        typos : Path | None, optional
            Executable supplied as ``TYPOS``; ``None`` selects the fake typos
            fixture.

        Returns
        -------
        subprocess.CompletedProcess[str]
            Completed process with captured standard output and error.
        """
        return run_process(
            [
                "make",
                "--no-print-directory",
                "spelling",
                f"GIT={self.git}",
                f"TYPOS={typos or self.typos}",
            ],
            cwd=self.directory,
            environment=self.environment,
        )

    def test_invokes_typos_once_with_the_explicit_policy_and_force_exclude(self) -> None:
        """Pass the explicit policy and force-exclude to typos exactly once."""
        source = self.directory / "tracked file.txt"
        source.write_text("colour\n", encoding="utf-8")
        self.write_tracked_paths(source.name)

        result = self.run_target()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_json_lines(self.git_log), [["ls-files", "-z", "--"]])
        self.assertEqual(
            read_json_lines(self.typos_log),
            [["--config", "typos.toml", "--force-exclude", "--", source.name]],
        )

    def test_excludes_untracked_and_ignored_paths(self) -> None:
        """Keep untracked and ignored paths out of the typos arguments."""
        tracked = self.directory / "tracked.txt"
        untracked = self.directory / "untracked.txt"
        ignored = self.directory / "ignored.txt"
        for path in (tracked, untracked, ignored):
            path.write_text("colour\n", encoding="utf-8")
        self.write_tracked_paths(tracked.name)

        result = self.run_target()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            read_json_lines(self.typos_log),
            [["--config", "typos.toml", "--force-exclude", "--", tracked.name]],
        )

    def test_preserves_spaces_and_newlines_in_tracked_paths(self) -> None:
        """Preserve spaces and newlines when passing NUL-separated paths."""
        spaced = self.directory / "space name.txt"
        newline = self.directory / "line\nbreak.txt"
        for path in (spaced, newline):
            path.write_text("colour\n", encoding="utf-8")
        self.write_tracked_paths(spaced.name, newline.name)

        result = self.run_target()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            read_json_lines(self.typos_log),
            [
                [
                    "--config",
                    "typos.toml",
                    "--force-exclude",
                    "--",
                    spaced.name,
                    newline.name,
                ]
            ],
        )

    def test_skips_deleted_only_and_lexically_last_tracked_paths(self) -> None:
        """Skip paths that no longer exist, including the lexically last one."""
        self.write_tracked_paths("deleted.txt")

        deleted_only = self.run_target()

        self.assertEqual(deleted_only.returncode, 0, deleted_only.stderr)
        self.assertEqual(read_json_lines(self.typos_log), [])

        self.typos_log.unlink(missing_ok=True)
        existing = self.directory / "existing.txt"
        existing.write_text("colour\n", encoding="utf-8")
        self.write_tracked_paths(existing.name, "z-deleted.txt")

        deleted_last = self.run_target()

        self.assertEqual(deleted_last.returncode, 0, deleted_last.stderr)
        self.assertEqual(
            read_json_lines(self.typos_log),
            [["--config", "typos.toml", "--force-exclude", "--", existing.name]],
        )

    def test_skips_an_empty_tracked_set_without_requiring_typos(self) -> None:
        """Succeed without invoking typos when no tracked path remains."""
        self.write_tracked_paths()
        result = self.run_target(typos=self.directory / "missing-typos")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_json_lines(self.typos_log), [])

    def test_fails_closed_when_git_discovery_fails(self) -> None:
        """Propagate a Git discovery failure instead of running typos."""
        self.environment["FAKE_GIT_STATUS"] = "23"

        result = self.run_target()

        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("controlled git failure", result.stderr)
        self.assertEqual(read_json_lines(self.typos_log), [])

    def test_propagates_typos_failure(self) -> None:
        """Propagate the typos exit status through the spelling target."""
        source = self.directory / "tracked.txt"
        source.write_text("colour\n", encoding="utf-8")
        self.write_tracked_paths(source.name)
        self.environment["FAKE_TYPOS_STATUS"] = "17"

        result = self.run_target()

        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(read_json_lines(self.typos_log)), 1)


if __name__ == "__main__":
    unittest.main()
