#!/usr/bin/env python3
"""Regression tests for production unwrap/expect path classification."""

from __future__ import annotations

from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import check_no_unwrap_expect_prod as scanner  # noqa: E402


class TestPathClassification(unittest.TestCase):
    """Check which paths the unwrap/expect scanner treats as test code."""
    def test_nested_test_support_module_is_excluded(self) -> None:
        """Check that a nested test support module is classified as test code."""
        support_path = (
            scanner.REPO_ROOT
            / "crates/codegen/diapason-acp/src/zed/agent/tool_config_tests/support.rs"
        )

        self.assertTrue(scanner.is_test_path(support_path))

    def test_production_rust_module_remains_scanned(self) -> None:
        """Check that a production Rust module is still scanned."""
        production_path = (
            scanner.REPO_ROOT
            / "crates/codegen/diapason-acp/src/zed/provider_runtime.rs"
        )

        self.assertFalse(scanner.is_test_path(production_path))

    def test_contests_module_is_not_mistaken_for_test_source(self) -> None:
        """Check that a module named contests.rs is not treated as test code."""
        production_path = scanner.REPO_ROOT / "crates/example/src/contests.rs"

        self.assertFalse(scanner.is_test_path(production_path))


if __name__ == "__main__":
    unittest.main()
