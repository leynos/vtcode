#!/usr/bin/env python3
"""Regression tests for configuration reference overrides."""

from __future__ import annotations

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

    with pytest.raises(RuntimeError, match="acp.audit.max_files"):
        generator.apply_reference_overrides(entries)
