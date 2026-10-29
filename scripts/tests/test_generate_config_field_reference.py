#!/usr/bin/env python3
"""Regression tests for the generated configuration reference renderer."""

from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT_PATH = (
    Path(__file__).resolve().parents[1] / "generate_config_field_reference.py"
)
SPEC = importlib.util.spec_from_file_location("generate_config_field_reference", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"Unable to import {SCRIPT_PATH}")
GENERATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = GENERATOR
SPEC.loader.exec_module(GENERATOR)
MDTABLEFIX = shutil.which("mdtablefix")


class ConfigFieldReferenceTests(unittest.TestCase):
    def test_override_requires_every_configured_schema_field(self) -> None:
        entries = [GENERATOR.FieldEntry(path="acp.audit.enabled", type_name="bool", required=False, default="false", description="")]
        with self.assertRaisesRegex(RuntimeError, re.escape("acp.audit.max_files")):
            GENERATOR.apply_reference_overrides(entries)

    def test_schema_export_inherits_compile_wrappers_without_changing_parent(self) -> None:
        original_environment = os.environ.copy()
        wrappers = {
            "RUSTC_WRAPPER": "/managed/build-limits-rustc",
            "RUSTDOC": "/managed/build-limits-rustdoc",
        }
        child_command = [
            sys.executable,
            "-c",
            "import json, os; "
            "print(json.dumps({key: os.environ.get(key) "
            "for key in ('RUSTC_WRAPPER', 'RUSTDOC')}))",
        ]
        with patch.dict(os.environ, wrappers):
            inherited_environment = os.environ.copy()
            with patch.object(GENERATOR, "CARGO_SCHEMA_CMD", child_command):
                exported = GENERATOR.load_schema_from_cargo()

            self.assertEqual(exported, wrappers)
            self.assertEqual(dict(os.environ), inherited_environment)

        self.assertEqual(dict(os.environ), original_environment)

    def test_long_default_remains_parseable_json(self) -> None:
        value = [
            f"entry-{index}-with-enough-text-to-exceed-the-old-limit"
            for index in range(4)
        ]

        rendered = GENERATOR.format_default(value)

        self.assertEqual(json.loads(rendered), value)

    def test_render_markdown_escapes_pipes_inside_backtick_safe_code_span(self) -> None:
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

        self.assertEqual(
            rows,
            [
                "| `example.path` | `string` | no | ``value\\|with`tick`` | "
                "Example value. |"
            ],
        )

    def test_groups_preserve_every_complete_field_once(self) -> None:
        entries = [
            GENERATOR.FieldEntry("agent", "object", True, "{}", "Agent root."),
            GENERATOR.FieldEntry(
                "agent.harness", "object", False, '{"steps": 12}', "Harness root."
            ),
            GENERATOR.FieldEntry(
                "agent.harness.steps",
                "integer | null",
                True,
                "12",
                "Sensitive: inherited | explicit `override`.",
            ),
            GENERATOR.FieldEntry(
                "agent.tools.command", "string", False, "`run``now`", "Tool command."
            ),
            GENERATOR.FieldEntry("zeta.root", "boolean", False, "", ""),
        ]

        lines = GENERATOR.render_markdown(entries).splitlines()
        rows = [
            line
            for line in lines
            if line.startswith("| ") and "| Field |" not in line
        ]

        self.assertEqual(
            rows,
            [
                "| `agent` | `object` | yes | `{}` | Agent root. |",
                '| `agent.harness` | `object` | no | `{"steps": 12}` | Harness root. |',
                "| `agent.harness.steps` | `integer \\| null` | yes | `12` | "
                "Sensitive: inherited \\| explicit `override`. |",
                "| `agent.tools.command` | `string` | no | ``` `run``now` ``` | "
                "Tool command. |",
                "| `zeta.root` | `boolean` | no | `-` | - |",
            ],
        )

    def test_group_headings_follow_collector_order_with_two_path_components(self) -> None:
        entries = [
            GENERATOR.FieldEntry(path, "string", False, "null", "")
            for path in (
                "agent",
                "agent.harness",
                "agent.harness.steps",
                "agent.tools.command",
                "zeta.root.leaf",
            )
        ]

        rendered = GENERATOR.render_markdown(entries)

        self.assertEqual(
            [line for line in rendered.splitlines() if line.startswith("## ")],
            [
                "## `agent`",
                "## `agent.harness`",
                "## `agent.tools`",
                "## `zeta.root`",
            ],
        )
        self.assertEqual(
            rendered.count("| Field | Type | Required | Default | Description |"), 4
        )
        self.assertEqual(
            rendered.count("|-------|------|----------|---------|-------------|"), 4
        )
        self.assertEqual(GENERATOR.render_markdown(entries), rendered)

    def test_long_structured_default_is_complete_in_its_group(self) -> None:
        value = {
            "commands": [f"command-{index}-with-long-arguments" for index in range(40)],
            "options": {"enabled": True, "retry": 3},
        }
        entries = [
            GENERATOR.FieldEntry(
                "automation.commands",
                "object",
                False,
                GENERATOR.format_default(value),
                "Commands.",
            ),
            GENERATOR.FieldEntry("ui.theme", "string", False, '"dark"', "Theme."),
        ]

        rendered = GENERATOR.render_markdown(entries)
        row = next(
            line
            for line in rendered.splitlines()
            if line.startswith("| `automation.commands`")
        )
        default_cell = row.split(" | ")[3]

        self.assertEqual(json.loads(default_cell.strip("`")), value)
        self.assertEqual(rendered.count('"command-39-with-long-arguments"'), 1)

    @unittest.skipUnless(MDTABLEFIX, "mdtablefix is required for the formatter regression")
    def test_formatted_reference_stays_below_file_limit_and_is_idempotent(self) -> None:
        long_value = {
            "commands": [f"command-{index:03d}-with-arguments" for index in range(80)]
        }
        long_default = GENERATOR.format_default(long_value)
        entries = [
            GENERATOR.FieldEntry(
                path=f"group{index // 10:03d}.settings.value_{index}",
                type_name="object" if index == 0 else "integer",
                required=False,
                default=long_default if index == 0 else str(index),
                description=f"Complete description for field {index}.",
            )
            for index in range(938)
        ]
        with tempfile.TemporaryDirectory(prefix="vtcode-reference-") as directory:
            reference = Path(directory) / "CONFIG_FIELD_REFERENCE.md"
            reference.write_text(GENERATOR.render_markdown(entries), encoding="utf-8")
            command = [
                MDTABLEFIX,
                "--in-place",
                "--wrap",
                "--renumber",
                "--breaks",
                "--ellipsis",
                "--fences",
                str(reference),
            ]
            subprocess.run(command, check=True, capture_output=True, text=True)
            formatted = reference.read_bytes()
            subprocess.run(command, check=True, capture_output=True, text=True)

            self.assertEqual(reference.read_bytes(), formatted)
            self.assertLess(len(formatted), 1_000_000)
            text = formatted.decode("utf-8")
            self.assertEqual(
                [
                    line.split("|", 2)[1].strip().strip("`")
                    for line in text.splitlines()
                    if line.startswith("| `group")
                ],
                [entry.path for entry in entries],
            )
            self.assertEqual(text.count(long_default), 1)


if __name__ == "__main__":
    unittest.main()
