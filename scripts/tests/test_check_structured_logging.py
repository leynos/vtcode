#!/usr/bin/env python3
"""Regression tests for structured-logging source path classification."""

from __future__ import annotations

from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import check_structured_logging as scanner  # noqa: E402


class TestPathClassification(unittest.TestCase):
    def test_test_module_name_requires_exact_path_component(self) -> None:
        test_path = scanner.REPO_ROOT / "crates/example/src/tests.rs"
        lookalike_path = scanner.REPO_ROOT / "crates/example/src/contests.rs"

        self.assertTrue(scanner.is_test_path(test_path))
        self.assertFalse(scanner.is_test_path(lookalike_path))


if __name__ == "__main__":
    unittest.main()
