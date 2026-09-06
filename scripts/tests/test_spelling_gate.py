"""Exercise the spelling gate's NUL-safe tracked-file discovery."""

from __future__ import annotations

import os
from pathlib import Path
import tempfile
import unittest

from spelling_gate_test_support import (
    prepare_make_repository,
    read_json_lines,
    run_process,
    write_fake_git,
    write_fake_typos,
)


class SpellingGateProcessTests(unittest.TestCase):
    """Keep the spelling gate scoped to existing, tracked repository paths."""

    def setUp(self) -> None:
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
        self.temporary_directory.cleanup()

    def write_tracked_paths(self, *paths: str) -> None:
        self.git_output.write_bytes(b"".join(path.encode() + b"\0" for path in paths))

    def run_target(self, *, typos: Path | None = None):
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
        self.write_tracked_paths()
        result = self.run_target(typos=self.directory / "missing-typos")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_json_lines(self.typos_log), [])

    def test_fails_closed_when_git_discovery_fails(self) -> None:
        self.environment["FAKE_GIT_STATUS"] = "23"

        result = self.run_target()

        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("controlled git failure", result.stderr)
        self.assertEqual(read_json_lines(self.typos_log), [])

    def test_propagates_typos_failure(self) -> None:
        source = self.directory / "tracked.txt"
        source.write_text("colour\n", encoding="utf-8")
        self.write_tracked_paths(source.name)
        self.environment["FAKE_TYPOS_STATUS"] = "17"

        result = self.run_target()

        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(read_json_lines(self.typos_log)), 1)


if __name__ == "__main__":
    unittest.main()
