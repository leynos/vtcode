#!/usr/bin/env python3
"""Regression tests for configuration reference overrides."""

from __future__ import annotations

import re
import json
from pathlib import Path
import sys

import pytest

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

    with pytest.raises(RuntimeError, match=re.escape("acp.audit.max_files")):
        generator.apply_reference_overrides(entries)


def test_format_code_span_pads_a_leading_backtick() -> None:
    assert generator.format_code_span("`value") == "`` `value ``"


def test_format_code_span_pads_a_trailing_backtick() -> None:
    assert generator.format_code_span("value`") == "`` value` ``"


def test_long_default_remains_parseable_json() -> None:
    value = [f"entry-{index}-with-enough-text-to-exceed-the-old-limit" for index in range(4)]

    rendered = generator.format_default(value)

    assert json.loads(rendered) == value


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

    assert rows == [
        "| `example.path` | `string` | no | ``value\\|with`tick`` | Example value. |"
    ]


if __name__ == "__main__":
    test_long_default_remains_parseable_json()
    test_render_markdown_escapes_pipes_inside_backtick_safe_code_span()
