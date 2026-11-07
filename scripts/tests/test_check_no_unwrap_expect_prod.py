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
    def test_nested_test_support_module_is_excluded(self) -> None:
        support_path = (
            scanner.REPO_ROOT
            / "crates/codegen/vtcode-acp/src/zed/agent/tool_config_tests/support.rs"
        )

        self.assertTrue(scanner.is_test_path(support_path))

    def test_cfg_test_path_module_is_excluded(self) -> None:
        support_path = (
            scanner.REPO_ROOT
            / "crates/codegen/vtcode-acp/src/zed/agent/rate_limit_test_support.rs"
        )

        self.assertTrue(scanner.is_test_path(support_path))

    def test_production_rust_module_remains_scanned(self) -> None:
        production_path = (
            scanner.REPO_ROOT
            / "crates/codegen/vtcode-acp/src/zed/provider_runtime.rs"
        )

        self.assertFalse(scanner.is_test_path(production_path))

    def test_contests_module_is_not_mistaken_for_test_source(self) -> None:
        production_path = scanner.REPO_ROOT / "crates/example/src/contests.rs"

        self.assertFalse(scanner.is_test_path(production_path))


class TestCfgTestPredicate(unittest.TestCase):
    def test_test_only_predicates_are_recognised(self) -> None:
        for line in ('#[cfg(test)]', '#[cfg(all(test, feature = "fixtures"))]'):
            with self.subTest(line=line):
                self.assertIsNotNone(scanner.CFG_TEST_RE.search(line))

    def test_production_predicates_are_not_test_only(self) -> None:
        for line in (
            '#[cfg(feature = "latest")]',
            '#[cfg(any(test, feature = "fixtures"))]',
            "#[cfg(not(test))]",
        ):
            with self.subTest(line=line):
                self.assertIsNone(scanner.CFG_TEST_RE.search(line))


if __name__ == "__main__":
    unittest.main()
