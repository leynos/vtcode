#!/usr/bin/env python3
"""Regression tests for configuration reference overrides."""

from __future__ import annotations

import itertools
import json
from pathlib import Path
import random
import re
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
    """A value that opens with a backtick must be padded inside the span."""
    assert generator.format_code_span("`value") == "`` `value ``", "a leading backtick must be padded"


def test_format_code_span_pads_a_trailing_backtick() -> None:
    """A value that closes with a backtick must be padded inside the span."""
    assert generator.format_code_span("value`") == "`` value` ``", "a trailing backtick must be padded"


def _split_rendered_code_span(rendered: str) -> tuple[int, str]:
    """Recover a rendered code span's delimiter length and inner content.

    This is an independent oracle: it reads the rendered string the way a
    Markdown parser does, rather than recomputing what ``format_code_span``
    did.
    """
    delimiter_length = len(rendered) - len(rendered.lstrip("`"))
    assert delimiter_length > 0, f"a code span must open with a backtick run: {rendered!r}"
    assert rendered.endswith("`" * delimiter_length), (
        f"a code span must close with the delimiter run it opened: {rendered!r}"
    )
    return delimiter_length, rendered[delimiter_length : len(rendered) - delimiter_length]


def _expected_cell_text(value: str) -> str:
    """Escape a table cell by the GFM rule, independently of the generator.

    A literal pipe inside a cell must be backslash-escaped, and a newline
    cannot appear in a cell. Stating the rule here rather than calling
    ``escape_cell`` means a helper that dropped a pipe instead of escaping it
    fails the test instead of changing the expectation along with the output.
    """
    return value.replace("|", "\\|").replace("\n", " ").strip()


# The characters that matter to the renderer: backticks (delimiter and
# boundary padding), pipes (table-cell escaping), spaces (padding) and ordinary
# text. Values are non-empty because GFM cannot express an empty inline code
# span, and every caller passes a field path, type name, or JSON-rendered
# default.
_SPAN_ALPHABET = ("`", "|", " ", "a")


def _span_cases() -> list[str]:
    """Return a deterministic corpus of code-span values.

    Short strings are enumerated exhaustively over the characters that matter
    to the renderer, so every combination of boundary backticks, pipes and
    padding up to length four is covered. Longer strings come from a seeded
    generator, so run length and position vary without an external dependency.
    """
    cases = [
        "".join(combination)
        for length in range(1, 5)
        for combination in itertools.product(_SPAN_ALPHABET, repeat=length)
    ]
    rng = random.Random(0x5EED)
    for _ in range(200):
        length = rng.randint(1, 40)
        cases.append("".join(rng.choice(_SPAN_ALPHABET) for _ in range(length)))
    return cases


def test_format_code_span_round_trips_and_cannot_close_early() -> None:
    """A rendered span must enclose its value without closing early.

    The oracle reads the rendered string the way a Markdown parser does, so a
    delimiter that appears inside the content, or padding that changes the
    value, fails the test.
    """
    for value in _span_cases():
        rendered = generator.format_code_span(value)
        delimiter_length, content = _split_rendered_code_span(rendered)

        assert "`" * delimiter_length not in content, (
            "the delimiter must be longer than every backtick run it encloses, or "
            f"the span closes early: {value!r} -> {rendered!r}"
        )

        expected_content = f" {value} " if value.startswith("`") or value.endswith("`") else value
        assert content == expected_content, (
            "a boundary backtick must be padded so it cannot extend the delimiter, "
            f"and every other value must survive unchanged: {value!r} -> {rendered!r}"
        )


def test_format_code_span_uses_the_shortest_safe_delimiter() -> None:
    """The delimiter must be exactly one backtick longer than the longest run.

    A shorter delimiter would close early; a longer one would render more
    backticks than the value needs.
    """
    for value in _span_cases():
        rendered = generator.format_code_span(value)
        delimiter_length, _content = _split_rendered_code_span(rendered)
        longest_run = max((len(match) for match in re.findall(r"`+", value)), default=0)

        assert delimiter_length == longest_run + 1, (
            "the delimiter must be exactly one backtick longer than the longest run "
            f"in the value: {value!r} -> {rendered!r}"
        )


def test_generated_table_cells_preserve_pipes_and_backticks() -> None:
    """A generated row must keep its five columns and its default value.

    The row is split on unescaped pipes, so a default containing a pipe must
    stay inside its cell. The default cell is then read with the same
    parser-style oracle, so a delimiter that closes early or padding that
    changes the value fails the test. The expected cell text comes from
    ``_expected_cell_text`` rather than ``escape_cell``, so a helper that
    dropped a pipe cannot satisfy the assertion by dropping it from the
    expectation too.
    """
    for value in _span_cases():
        escaped = _expected_cell_text(value)
        if not escaped:
            # A whitespace-only default strips to nothing, which GFM cannot
            # express as an inline code span; that case is out of scope here.
            continue
        entry = generator.FieldEntry(
            path="example.path",
            type_name="string",
            required=False,
            default=value,
            description="Example value.",
        )
        row = next(
            line
            for line in generator.render_markdown([entry]).splitlines()
            if line.startswith("| `example.path`")
        )
        cells = [cell.strip() for cell in re.split(r"(?<!\\)\|", row)[1:-1]]

        assert len(cells) == 5, f"a pipe in the default must not add a column: {value!r} -> {row!r}"
        assert cells[3].count("|") == value.count("|"), (
            "a generated cell must keep every pipe in the default: "
            f"{value!r} -> {row!r}"
        )
        assert cells[3].count("\\|") == value.count("|"), (
            "every pipe in a generated cell must be backslash-escaped: "
            f"{value!r} -> {row!r}"
        )

        delimiter_length, content = _split_rendered_code_span(cells[3])
        expected_content = f" {escaped} " if escaped.startswith("`") or escaped.endswith("`") else escaped
        assert content == expected_content, (
            "a generated table cell must render the escaped default it was given: "
            f"{value!r} -> {row!r}"
        )
        assert "`" * delimiter_length not in content, (
            "the default cell's delimiter must not appear inside its content: "
            f"{value!r} -> {row!r}"
        )


def test_long_default_remains_parseable_json() -> None:
    """A default too long for one line must still parse as JSON."""
    value = [f"entry-{index}-with-enough-text-to-exceed-the-old-limit" for index in range(4)]

    rendered = generator.format_default(value)

    assert json.loads(rendered) == value, "a long default must stay parseable as JSON"


def test_render_markdown_escapes_pipes_inside_backtick_safe_code_span() -> None:
    """A pipe stays escaped even when the value also needs a wider delimiter."""
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
