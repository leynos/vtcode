#!/usr/bin/env python3
"""Regression tests for configuration reference overrides."""

from __future__ import annotations

import json
from pathlib import Path
import re
import sys

from hypothesis import given, strategies as st

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


# Fragments bias the strategy towards backtick runs at both boundaries and in
# the middle, alongside pipes and spaces. Values are non-empty because GFM
# cannot express an empty inline code span (a lone backtick string has no
# matching closer), and every caller passes a field path, type name, or
# JSON-rendered default.
_SPAN_FRAGMENTS = st.lists(
    st.sampled_from(["`", "``", "```", "````", "text", "|", " "]),
    min_size=1,
    max_size=12,
).map("".join)
_SPAN_TEXT = st.one_of(
    _SPAN_FRAGMENTS,
    st.text(alphabet=st.characters(blacklist_characters="\n\r"), min_size=1, max_size=40),
)


@given(_SPAN_TEXT)
def test_format_code_span_round_trips_and_cannot_close_early(value: str) -> None:
    """A rendered span must enclose its value without closing early.

    The oracle reads the rendered string the way a Markdown parser does, so a
    delimiter that appears inside the content, or padding that changes the
    value, fails the test.
    """
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


@given(_SPAN_TEXT)
def test_format_code_span_uses_the_shortest_safe_delimiter(value: str) -> None:
    """The delimiter must be exactly one backtick longer than the longest run.

    A shorter delimiter would close early; a longer one would render more
    backticks than the value needs.
    """
    rendered = generator.format_code_span(value)
    delimiter_length, _content = _split_rendered_code_span(rendered)
    longest_run = max((len(match) for match in re.findall(r"`+", value)), default=0)

    assert delimiter_length == longest_run + 1, (
        "the delimiter must be exactly one backtick longer than the longest run "
        f"in the value: {value!r} -> {rendered!r}"
    )


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
