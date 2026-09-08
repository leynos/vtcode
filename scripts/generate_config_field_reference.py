#!/usr/bin/env python3
"""Generate docs/config/CONFIG_FIELD_REFERENCE.md from vtcode-config schema."""

from __future__ import annotations

import argparse
import json
import os
import struct
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = REPO_ROOT / "docs" / "config" / "CONFIG_FIELD_REFERENCE.md"
CARGO_SCHEMA_CMD = [
    "cargo",
    "run",
    "--locked",
    "-q",
    "-p",
    "vtcode-config",
    "--features",
    "schema",
    "--example",
    "schema_dump",
]


@dataclass
class FieldEntry:
    path: str
    type_name: str
    required: bool
    default: str
    description: str


_MAX_WALK_DEPTH = 32


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate config field reference docs from vtcode-config schema."
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help=f"Output markdown file path (default: {DEFAULT_OUTPUT}).",
    )
    parser.add_argument(
        "--schema",
        type=Path,
        help="Read a previously captured schema JSON instead of invoking Cargo.",
    )
    return parser.parse_args()


def load_schema_from_cargo() -> dict[str, Any]:
    result = subprocess.run(
        CARGO_SCHEMA_CMD,
        cwd=REPO_ROOT,
        env={**os.environ, "RUSTC_WRAPPER": ""},
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        stderr = result.stderr.strip()
        raise RuntimeError(
            "Failed to export configuration schema.\n"
            "Remediation:\n"
            "1. Ensure Rust toolchain is installed and `cargo` is available.\n"
            "2. Run: cargo run --locked -p vtcode-config --features schema --example schema_dump\n"
            f"3. Cargo stderr:\n{stderr}"
        )

    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            "Schema export output was not valid JSON.\n"
            "Remediation:\n"
            "1. Re-run cargo command directly to inspect output.\n"
            "2. Ensure vtcode-config example `schema_dump` prints JSON only."
        ) from exc


def load_schema_from_snapshot(schema_path: Path) -> dict[str, Any]:
    try:
        return json.loads(schema_path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise RuntimeError(
            f"Failed to read schema snapshot {schema_path}: {exc}"
        ) from exc
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"Schema snapshot {schema_path} was not valid JSON: {exc}"
        ) from exc


def decode_json_pointer_segment(segment: str) -> str:
    return segment.replace("~1", "/").replace("~0", "~")


def resolve_pointer(document: dict[str, Any], pointer: str) -> Any:
    if not pointer.startswith("#/"):
        raise KeyError(f"Unsupported schema reference pointer: {pointer}")
    current: Any = document
    for segment in pointer[2:].split("/"):
        key = decode_json_pointer_segment(segment)
        current = current[key]
    return current


def deep_copy(value: Any) -> Any:
    return json.loads(json.dumps(value))


def merge_schema(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    merged = deep_copy(base)
    for key, value in overlay.items():
        if key == "$ref":
            continue
        if key == "required":
            existing = set(merged.get("required", []))
            existing.update(value)
            merged["required"] = sorted(existing)
            continue
        if key == "properties":
            props = merged.setdefault("properties", {})
            props.update(value)
            continue
        merged[key] = deep_copy(value)
    return merged


def normalize_schema_node(
    node: dict[str, Any],
    root_schema: dict[str, Any],
    ref_stack: tuple[str, ...] = (),
) -> dict[str, Any]:
    normalized = deep_copy(node)

    while "$ref" in normalized:
        ref = normalized["$ref"]
        if ref in ref_stack:
            # Recursive schema branch; stop expansion here.
            return {k: v for k, v in normalized.items() if k != "$ref"}
        target = resolve_pointer(root_schema, ref)
        normalized = merge_schema(target, normalized)
        ref_stack = (*ref_stack, ref)

    if "allOf" in normalized:
        all_of = normalized.pop("allOf")
        base = normalized
        for sub in all_of:
            sub_normalized = normalize_schema_node(sub, root_schema, ref_stack)
            base = merge_schema(base, sub_normalized)
        normalized = base

    return normalized


def _normalise_home_prefix(value: Any, home_dir: str) -> Any:
    home_root = os.path.normpath(home_dir)
    if isinstance(value, str):
        if value == home_root:
            return "$HOME"
        prefix = f"{home_root}{os.sep}"
        if value.startswith(prefix):
            return f"$HOME{value[len(home_root) :]}"
        return value
    if isinstance(value, list):
        return [_normalise_home_prefix(item, home_dir) for item in value]
    if isinstance(value, dict):
        return {
            key: _normalise_home_prefix(item, home_dir) for key, item in value.items()
        }
    return value


def format_default(
    value: Any, float_format: str | None = None, home_dir: str | None = None
) -> str:
    value = _normalise_home_prefix(value, home_dir or os.path.expanduser("~"))
    return _format_default_value(value, float_format)


def _format_default_value(value: Any, float_format: str | None) -> str:
    if isinstance(value, float) and float_format == "float":
        # schemars widens `f32` defaults to `f64`, leaving binary tails like
        # 0.699999988079071 for 0.7. Render the shortest decimal that still
        # round-trips to the same float32.
        shortened = _shortest_f32_text(value)
        if shortened is not None:
            text = shortened
        else:
            text = json.dumps(value, ensure_ascii=True)
    else:
        text = json.dumps(value, ensure_ascii=True)

    return text


def _shortest_f32_text(value: float) -> str | None:
    try:
        packed = struct.pack("<f", value)
    except OverflowError:
        return None
    for precision in range(1, 10):
        text = f"{value:.{precision}g}"
        try:
            if struct.pack("<f", float(text)) == packed:
                if "." not in text and "e" not in text and "E" not in text:
                    text = f"{text}.0"
                return text
        except OverflowError:
            return None
    return None


def format_type_name(node: dict[str, Any]) -> str:
    type_value = node.get("type")
    if isinstance(type_value, list):
        return " | ".join(sorted(str(item) for item in type_value))
    if isinstance(type_value, str):
        return type_value

    if "enum" in node:
        values = [json.dumps(value, ensure_ascii=True) for value in node["enum"]]
        preview = ", ".join(values[:4])
        if len(values) > 4:
            preview = f"{preview}, ..."
        return f"enum({preview})"

    variants: list[str] = []
    for key in ("oneOf", "anyOf"):
        for option in node.get(key, []):
            option_type = option.get("type")
            if isinstance(option_type, str):
                variants.append(option_type)
            elif "enum" in option:
                variants.append("enum")
            elif "$ref" in option:
                variants.append(option["$ref"].split("/")[-1])
    if variants:
        return " | ".join(sorted(set(variants)))

    if "properties" in node:
        return "object"
    if "items" in node:
        return "array"
    if "additionalProperties" in node:
        return "map"

    return "unknown"


def normalize_description(text: str | None) -> str:
    if not text:
        return ""
    return " ".join(text.strip().split())


def _upsert_field(field_map: dict[str, FieldEntry], entry: FieldEntry) -> None:
    existing = field_map.get(entry.path)
    if existing is None:
        field_map[entry.path] = entry
        return

    description = existing.description
    if not description and entry.description:
        description = entry.description
    default = existing.default
    if default == "" and entry.default:
        default = entry.default
    type_name = existing.type_name
    if type_name == "unknown" and entry.type_name != "unknown":
        type_name = entry.type_name
    field_map[entry.path] = FieldEntry(
        path=existing.path,
        type_name=type_name,
        required=existing.required or entry.required,
        default=default,
        description=description,
    )


def _build_field_entry(
    node: dict[str, Any],
    path: str,
    required: bool,
    root_schema: dict[str, Any],
) -> tuple[dict[str, Any], FieldEntry]:
    normalized = normalize_schema_node(node, root_schema)
    description = normalize_description(normalized.get("description"))
    default = (
        format_default(normalized["default"], normalized.get("format"))
        if "default" in normalized
        else ""
    )
    entry = FieldEntry(
        path=path,
        type_name=format_type_name(normalized),
        required=required,
        default=default,
        description=description,
    )
    return normalized, entry


def _is_union_schema(node: dict[str, Any]) -> bool:
    return "oneOf" in node or "anyOf" in node


def _is_object_schema(node: dict[str, Any]) -> bool:
    return node.get("type") == "object" or "properties" in node


def _is_array_schema(node: dict[str, Any]) -> bool:
    return node.get("type") == "array" or "items" in node


def _walk_properties(
    node: dict[str, Any],
    path: str,
    depth: int,
    parent_required: bool,
    root_schema: dict[str, Any],
    field_map: dict[str, FieldEntry],
) -> None:
    properties = node.get("properties", {})
    required_set = set(node.get("required", []))
    for prop_name in sorted(properties):
        child_path = f"{path}.{prop_name}" if path else prop_name
        _walk_field(
            properties[prop_name],
            child_path,
            parent_required and prop_name in required_set,
            depth,
            root_schema,
            field_map,
        )


def _union_object_branches(
    node: dict[str, Any], root_schema: dict[str, Any]
) -> list[dict[str, Any]]:
    branches = [node] if "properties" in node else []
    for key in ("oneOf", "anyOf"):
        for option in node.get(key, []):
            resolved = normalize_schema_node(option, root_schema)
            if _is_object_schema(resolved):
                branches.append(resolved)
    return branches


def _walk_union(
    node: dict[str, Any],
    entry: FieldEntry,
    path: str,
    required: bool,
    depth: int,
    root_schema: dict[str, Any],
    field_map: dict[str, FieldEntry],
) -> None:
    _upsert_field(field_map, entry)
    # Optional/enum-wrapped objects (`Option<T>`, untagged variants) still
    # define concrete child fields; walk their object branches.
    for branch in _union_object_branches(node, root_schema):
        _walk_properties(
            branch,
            path,
            depth + 1,
            required or not path,
            root_schema,
            field_map,
        )


def _walk_additional_properties(
    node: dict[str, Any],
    path: str,
    depth: int,
    root_schema: dict[str, Any],
    field_map: dict[str, FieldEntry],
) -> None:
    additional = node.get("additionalProperties")
    if isinstance(additional, dict):
        map_path = f"{path}.*" if path else "*"
        _walk_field(
            additional,
            map_path,
            required=False,
            depth=depth + 1,
            root_schema=root_schema,
            field_map=field_map,
        )
    elif additional is True and path:
        _upsert_field(
            field_map,
            FieldEntry(
                path=f"{path}.*",
                type_name="any",
                required=False,
                default="",
                description="Additional map entries.",
            ),
        )


def _walk_object(
    node: dict[str, Any],
    entry: FieldEntry,
    path: str,
    required: bool,
    depth: int,
    root_schema: dict[str, Any],
    field_map: dict[str, FieldEntry],
) -> None:
    properties = node.get("properties", {})
    if not properties:
        _upsert_field(field_map, entry)
    _walk_properties(
        node,
        path,
        depth + 1,
        required or not path,
        root_schema,
        field_map,
    )
    _walk_additional_properties(node, path, depth, root_schema, field_map)


def _walk_array(
    node: dict[str, Any],
    entry: FieldEntry,
    path: str,
    depth: int,
    root_schema: dict[str, Any],
    field_map: dict[str, FieldEntry],
) -> None:
    _upsert_field(field_map, entry)
    items = node.get("items")
    if isinstance(items, dict):
        _walk_field(
            items,
            f"{path}[]",
            required=False,
            depth=depth + 1,
            root_schema=root_schema,
            field_map=field_map,
        )


def _walk_field(
    node: dict[str, Any],
    path: str,
    required: bool,
    depth: int,
    root_schema: dict[str, Any],
    field_map: dict[str, FieldEntry],
) -> None:
    # Real config nesting stays under ~10 levels; the cap only guards against
    # pathological self-referential `$ref` schemas.
    if depth > _MAX_WALK_DEPTH:
        return
    normalized, entry = _build_field_entry(node, path, required, root_schema)
    if _is_union_schema(normalized):
        _walk_union(
            normalized,
            entry,
            path,
            required,
            depth,
            root_schema,
            field_map,
        )
        return
    if _is_object_schema(normalized):
        _walk_object(
            normalized,
            entry,
            path,
            required,
            depth,
            root_schema,
            field_map,
        )
        return
    if _is_array_schema(normalized):
        _walk_array(normalized, entry, path, depth, root_schema, field_map)
        return
    _upsert_field(field_map, entry)


def collect_fields(root_schema: dict[str, Any]) -> list[FieldEntry]:
    field_map: dict[str, FieldEntry] = {}
    _walk_field(
        root_schema,
        "",
        required=False,
        depth=0,
        root_schema=root_schema,
        field_map=field_map,
    )
    entries = [entry for entry in field_map.values() if entry.path]
    entries.sort(key=lambda item: item.path)
    return entries


def escape_cell(value: str) -> str:
    return value.replace("|", "\\|").replace("\n", " ").strip()


def render_markdown(entries: list[FieldEntry]) -> str:
    lines = [
        "# Config Field Reference",
        "",
        "Generated from `vtcode-config` schema (`VTCodeConfig`) for complete field coverage.",
        "",
        "Regenerate:",
        "",
        "```bash",
        "python3 scripts/generate_config_field_reference.py",
        "make fmt",
        "```",
        "",
        "| Field | Type | Required | Default | Description |",
        "|-------|------|----------|---------|-------------|",
    ]
    for entry in entries:
        required = "yes" if entry.required else "no"
        default = entry.default or "-"
        description = entry.description or "-"
        lines.append(
            f"| `{escape_cell(entry.path)}` | `{escape_cell(entry.type_name)}` | "
            f"{required} | `{escape_cell(default)}` | {escape_cell(description)} |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    output_path = args.output
    if not output_path.is_absolute():
        output_path = REPO_ROOT / output_path

    try:
        if args.schema is None:
            schema = load_schema_from_cargo()
        else:
            schema_path = args.schema
            if not schema_path.is_absolute():
                schema_path = REPO_ROOT / schema_path
            schema = load_schema_from_snapshot(schema_path)
        entries = collect_fields(schema)
        markdown = render_markdown(entries)
    except RuntimeError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(markdown, encoding="utf-8")
    try:
        display_path = output_path.relative_to(REPO_ROOT)
    except ValueError:
        display_path = output_path
    print(f"Wrote {len(entries)} config fields to {display_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
