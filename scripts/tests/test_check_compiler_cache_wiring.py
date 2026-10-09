#!/usr/bin/env python3
"""Behavioural tests for the compiler-cache wiring check.

Each mutation breaks the repository's real workflow in one way and must fail
the named case; the unmutated workflow must pass.
"""

from __future__ import annotations

import copy
from pathlib import Path
import sys
import unittest

import yaml

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import check_compiler_cache_wiring as wiring  # noqa: E402

ROOT = SCRIPTS.parent


def load(name: str) -> dict:
    return yaml.safe_load((ROOT / ".github" / "workflows" / name).read_text("utf-8"))


def setup_step(workflow: dict, job: str) -> dict:
    return next(
        step
        for step in workflow["jobs"][job]["steps"]
        if str(step.get("uses", "")).startswith(wiring.SETUP_RUST)
    )


class CompilerCacheWiringTest(unittest.TestCase):
    def setUp(self) -> None:
        self.ci = copy.deepcopy(load("ci.yml"))

    def assert_fails(self, needle: str) -> None:
        found = wiring.violations("ci.yml", self.ci)
        self.assertTrue(
            any(needle in line for line in found), f"{needle!r} not in {found}"
        )

    def test_the_repository_workflows_pass(self) -> None:
        for name in wiring.REQUIRED_JOBS:
            self.assertEqual(wiring.violations(name, load(name)), [])

    def test_a_missing_setup_rust_step_fails(self) -> None:
        job = self.ci["jobs"]["lint-clippy"]
        job["steps"] = [s for s in job["steps"] if s is not setup_step(self.ci, "lint-clippy")]
        self.assert_fails("has no setup-rust step")

    def test_an_unpinned_reference_fails(self) -> None:
        setup_step(self.ci, "test")["uses"] = f"{wiring.SETUP_RUST}@main"
        self.assert_fails("pinned to a 40-character SHA")

    def test_a_job_level_wrapper_fails(self) -> None:
        self.ci["jobs"]["test"].setdefault("env", {})["RUSTC_WRAPPER"] = "sccache"
        self.assert_fails("sets RUSTC_WRAPPER")

    def test_a_workflow_level_sccache_variable_fails(self) -> None:
        self.ci.setdefault("env", {})["SCCACHE_GHA_ENABLED"] = "true"
        self.assert_fails("sets SCCACHE_GHA_ENABLED")

    def test_a_hand_rolled_sccache_step_fails(self) -> None:
        self.ci["jobs"]["check-cross-platform"]["steps"].append(
            {"uses": "mozilla-actions/sccache-action@" + "0" * 40}
        )
        self.assert_fails("installs mozilla-actions/sccache-action by hand")

    def test_a_hand_rolled_toolchain_step_fails(self) -> None:
        self.ci["jobs"]["lint-clippy"]["steps"].append(
            {"uses": "dtolnay/rust-toolchain@" + "0" * 40}
        )
        self.assert_fails("installs dtolnay/rust-toolchain by hand")

    def test_an_explicit_toolchain_input_fails(self) -> None:
        setup_step(self.ci, "test")["with"] = {"toolchain": "stable"}
        self.assert_fails("must not pass toolchain")

    def test_a_missing_job_fails(self) -> None:
        del self.ci["jobs"]["check-cross-platform"]
        self.assert_fails("job is missing")


if __name__ == "__main__":
    unittest.main()
