#!/usr/bin/env python3
"""Regression tests for structured-logging source path classification."""

from __future__ import annotations

from pathlib import Path
import sys

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import check_structured_logging as scanner  # noqa: E402 -- Import after adding scripts to sys.path.


def test_test_module_name_requires_exact_path_component() -> None:
    test_path = scanner.REPO_ROOT / "crates/example/src/tests.rs"
    lookalike_path = scanner.REPO_ROOT / "crates/example/src/contests.rs"

    assert scanner.is_test_path(test_path), "tests.rs should be classified as a test module"
    assert not scanner.is_test_path(lookalike_path), (
        "contests.rs should not be classified as a test module"
    )
