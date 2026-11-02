#!/usr/bin/env python3
"""Regression tests for configuration reference overrides."""

from __future__ import annotations

import json
from pathlib import Path
import sys

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import generate_config_field_reference as generator  # noqa: E402 -- Import after adding scripts to sys.path.


def test_override_requires_every_configured_schema_field() -> None:
    entries = [
        generator.FieldEntry(
            path="acp.audit.enabled",
            type_name="bool",
            required=False,
            default="false",
            description="",
        )
    ]

    try:
        generator.apply_reference_overrides(entries)
    except RuntimeError as error:
        assert "acp.audit.max_files" in str(error), (
            f"the error must name the missing field, got: {error}"
        )
    else:
        raise AssertionError("an incomplete override set must be rejected")


def test_format_code_span_pads_a_leading_backtick() -> None:
    assert generator.format_code_span("`value") == "`` `value ``", "a leading backtick must be padded"


def test_format_code_span_pads_a_trailing_backtick() -> None:
    assert generator.format_code_span("value`") == "`` value` ``", "a trailing backtick must be padded"


def test_long_default_remains_parseable_json() -> None:
    value = [f"entry-{index}-with-enough-text-to-exceed-the-old-limit" for index in range(4)]

    rendered = generator.format_default(value)

    assert json.loads(rendered) == value, "a long default must stay parseable as JSON"


def test_render_markdown_escapes_pipes_inside_backtick_safe_code_span() -> None:
    entry = generator.FieldEntry(
        path="example.path",
        type_name="string",
        required=False,
        default="value|with`tick",
        description="Example value.",
    )

    rows = [
        line
        for line in generator.render_markdown([entry]).splitlines()
        if line.startswith("| `example.path`")
    ]

    expected_rows = ["| `example.path` | `string` | no | ``value\\|with`tick`` | Example value. |"]
    assert rows == expected_rows, "a pipe inside a backtick-safe span must stay escaped"


if __name__ == "__main__":
    # Run every test in this module. Calling a hand-picked subset silently
    # skipped the tests added later, so a direct run could report success
    # while the rest of the suite failed.
    for name, test in sorted(globals().items()):
        if name.startswith("test_") and callable(test):
            test()
    print("all tests passed")
