#!/usr/bin/env python3
"""Structural tests for the Coverage workflow's proptest seed recording.

A SIGSEGV on the hosted run cannot be shrunk or persisted by proptest, so the
only durable way to replay it is to record the seed before the tests run. These
tests pin the invariants that make that record trustworthy, and each mutation
below breaks the real workflow in one way and must fail the named case.
"""

from __future__ import annotations

import copy
from pathlib import Path
import re
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
COVERAGE = ROOT / ".github" / "workflows" / "coverage.yml"

SEED_STEP = "Record proptest seed"
COVERAGE_STEP = "Generate code coverage"
COVERAGE_COMMAND = (
    "cargo llvm-cov nextest --locked --workspace --lcov --output-path lcov.info"
)
DECIMAL_GUARD = re.compile(r'\[\[\s*"\$proptest_rng_seed"\s*=~\s*\^\[0-9\]\+\$\s*\]\]')


def load() -> dict:
    """Parse coverage.yml, tolerating the YAML 1.1 ``on`` key."""
    workflow = yaml.safe_load(COVERAGE.read_text("utf-8"))
    # PyYAML reads the unquoted ``on`` trigger as the boolean True. Normalise
    # it so callers see the key GitHub Actions actually uses.
    if True in workflow:
        workflow["on"] = workflow.pop(True)
    return workflow


def steps(workflow: dict) -> list[dict]:
    """Return the coverage job's step list."""
    return workflow["jobs"]["coverage"]["steps"]


def step_named(workflow: dict, name: str) -> dict:
    """Return the first step with ``name``."""
    return next(step for step in steps(workflow) if step.get("name") == name)


def seed_steps(workflow: dict) -> list[dict]:
    """Return every step whose script mentions ``PROPTEST_RNG_SEED``."""
    return [step for step in steps(workflow) if "PROPTEST_RNG_SEED" in str(step.get("run", ""))]


def seed_content_violations(step: dict) -> list[str]:
    """Check that the seed step really exports one guarded decimal value."""
    found: list[str] = []
    run = str(step.get("run", ""))
    if "$GITHUB_ENV" not in run:
        found.append("the seed step does not export PROPTEST_RNG_SEED through $GITHUB_ENV")
    if "$GITHUB_STEP_SUMMARY" not in run:
        found.append("the seed step does not report the seed in $GITHUB_STEP_SUMMARY")
    if not DECIMAL_GUARD.search(run):
        found.append("the seed step does not reject non-decimal values")
    return found


def violations(workflow: dict) -> list[str]:
    found: list[str] = []
    seeds = seed_steps(workflow)
    if len(seeds) != 1:
        found.append(
            f"expected exactly one step referencing PROPTEST_RNG_SEED, found {len(seeds)}"
        )
        return found

    found.extend(seed_content_violations(seeds[0]))

    names = [step.get("name") for step in steps(workflow)]
    if SEED_STEP not in names:
        found.append(f"the {SEED_STEP!r} step is missing")
    elif COVERAGE_STEP not in names:
        found.append(f"the {COVERAGE_STEP!r} step is missing")
    else:
        # Compare the position of the step that actually writes the seed, not
        # an earlier placeholder with the same name.
        seed_position = steps(workflow).index(seeds[0])
        coverage_position = names.index(COVERAGE_STEP)
        if seed_position > coverage_position:
            found.append("the seed step must run before the coverage step")

    if COVERAGE_STEP in names:
        run_value = str(step_named(workflow, COVERAGE_STEP).get("run", "")).strip()
        if run_value != COVERAGE_COMMAND:
            found.append("the coverage command no longer matches the expected invocation")

    return found


class CoverageProptestSeedTest(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = copy.deepcopy(load())

    def assert_fails(self, needle: str) -> None:
        found = violations(self.workflow)
        self.assertTrue(any(needle in line for line in found), f"{needle!r} not in {found}")

    def test_the_repository_workflow_passes(self) -> None:
        self.assertEqual(violations(load()), [])

    def test_the_trigger_key_is_normalised(self) -> None:
        self.assertIn("on", load())

    def test_a_missing_seed_step_fails(self) -> None:
        self.workflow["jobs"]["coverage"]["steps"] = [
            step for step in steps(self.workflow) if "PROPTEST_RNG_SEED" not in str(step.get("run", ""))
        ]
        self.assert_fails("expected exactly one step referencing PROPTEST_RNG_SEED")

    def test_a_duplicate_seed_step_fails(self) -> None:
        steps(self.workflow).append(copy.deepcopy(step_named(self.workflow, SEED_STEP)))
        self.assert_fails("expected exactly one step referencing PROPTEST_RNG_SEED")

    def test_seed_step_after_coverage_fails(self) -> None:
        steps_list = steps(self.workflow)
        seed = step_named(self.workflow, SEED_STEP)
        steps_list.remove(seed)
        steps_list.append(seed)
        self.assert_fails("must run before the coverage step")

    def test_seed_writer_after_coverage_fails_behind_a_placeholder(self) -> None:
        steps_list = steps(self.workflow)
        seed = step_named(self.workflow, SEED_STEP)
        steps_list.remove(seed)
        steps_list.insert(0, {"name": SEED_STEP, "run": "true"})
        steps_list.append(seed)
        self.assert_fails("must run before the coverage step")

    def test_missing_summary_write_fails(self) -> None:
        step_named(self.workflow, SEED_STEP)["run"] = step_named(self.workflow, SEED_STEP)[
            "run"
        ].replace("$GITHUB_STEP_SUMMARY", "$GITHUB_OUTPUT")
        self.assert_fails("does not report the seed in $GITHUB_STEP_SUMMARY")

    def test_missing_env_write_fails(self) -> None:
        step_named(self.workflow, SEED_STEP)["run"] = step_named(self.workflow, SEED_STEP)[
            "run"
        ].replace("$GITHUB_ENV", "$GITHUB_OUTPUT")
        self.assert_fails("does not export PROPTEST_RNG_SEED through $GITHUB_ENV")

    def test_missing_decimal_guard_fails(self) -> None:
        step = step_named(self.workflow, SEED_STEP)
        step["run"] = "\n".join(
            line for line in step["run"].splitlines() if "=~" not in line
        )
        self.assert_fails("does not reject non-decimal values")

    def test_an_altered_coverage_command_fails(self) -> None:
        step_named(self.workflow, COVERAGE_STEP)["run"] += " --no-fail-fast"
        self.assert_fails("coverage command no longer matches")

    def test_a_missing_coverage_command_fails(self) -> None:
        step_named(self.workflow, COVERAGE_STEP)["run"] = "cargo test"
        self.assert_fails("coverage command no longer matches")


if __name__ == "__main__":
    unittest.main()