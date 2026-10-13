#!/usr/bin/env python3
"""Regression tests for the generated configuration reference renderer."""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path


SCRIPT_PATH = (
    Path(__file__).resolve().parents[1] / "generate_config_field_reference.py"
)
SPEC = importlib.util.spec_from_file_location("generate_config_field_reference", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"Unable to import {SCRIPT_PATH}")
GENERATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = GENERATOR
SPEC.loader.exec_module(GENERATOR)


def test_long_default_remains_parseable_json() -> None:
    value = [f"entry-{index}-with-enough-text-to-exceed-the-old-limit" for index in range(4)]

    rendered = GENERATOR.format_default(value)

    assert json.loads(rendered) == value


def test_render_markdown_escapes_pipes_inside_backtick_safe_code_span() -> None:
    entry = GENERATOR.FieldEntry(
        path="example.path",
        type_name="string",
        required=False,
        default="value|with`tick",
        description="Example value.",
    )

    rows = [
        line
        for line in GENERATOR.render_markdown([entry]).splitlines()
        if line.startswith("| `example.path`")
    ]

    assert rows == [
        "| `example.path` | `string` | no | ``value\\|with`tick`` | Example value. |"
    ]


if __name__ == "__main__":
    test_long_default_remains_parseable_json()
    test_render_markdown_escapes_pipes_inside_backtick_safe_code_span()
