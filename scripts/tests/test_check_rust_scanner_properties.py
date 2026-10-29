#!/usr/bin/env python3
"""Fixed-seed invariants with independent call-position and graph oracles."""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass, field
import json
from pathlib import Path
import random
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from test_check_no_unwrap_expect_prod import CHECKER


_NOISE_SEED = 0x164A
_SCOPE_SEED = 0x164B
_GRAPH_SEED = 0x164C
_BAIT = (
    '#[cfg(test)]', '#[tokio::test]', 'mod tests {', '.unwrap()',
    '.expect("bait")', '}]>)', '{[<(', '=> , ;', '//', '/* inner */',
    '\\"', '\n', 'λ', 'plain words', 'r###"', '"###',
)


@dataclass(frozen=True)
class _SourceCase:
    source: str
    expected_lines: list[int]


@dataclass(frozen=True)
class _ScopeFixture:
    source: str
    test_line: int
    production_line: int


@contextmanager
def _repository() -> Iterator[Path]:
    """Give each generated example fresh files and a restored entry-point root."""
    with TemporaryDirectory() as directory:
        root = Path(directory)
        with patch.object(CHECKER, "REPO_ROOT", root):
            yield root


def _noise(rng: random.Random) -> str:
    """Emit inert Rust source containing arbitrary delimiter and panic-call bait."""
    payload = " ".join(rng.choices(_BAIT, k=rng.randint(2, 24)))
    kind = rng.randrange(5)
    if kind == 0:
        return "// " + payload.replace("\n", " ") + "\n"
    if kind == 1:
        depth = rng.randint(1, 6)
        body = payload.replace("/*", "/ *").replace("*/", "* /")
        return "/* " * depth + body + " */" * depth + "\n"
    if kind == 2:
        return "const NOISE: &str = " + json.dumps(payload, ensure_ascii=False) + ";\n"
    if kind == 3:
        return "const NOISE: char = " + rng.choice(("'{'", "'}'", "'λ'", "'\\\\'")) + ";\n"
    return _raw_noise(rng, payload)


def _raw_noise(rng: random.Random, payload: str) -> str:
    hashes = "#" * rng.randint(1, 6)
    while '"' + hashes in payload:
        hashes += "#"
    return f'const NOISE: &str = r{hashes}"{payload}"{hashes};\n'


def _scan_source(source: str) -> list[int]:
    with _repository() as root:
        path = root / "lib.rs"
        path.write_text(source, encoding="utf-8", newline="")
        return [line for _, line, _ in CHECKER.scan_file(path, [])]


def _production_template(rng: random.Random) -> _SourceCase:
    """Mark actual call lines while constructing the template, without parsing it."""
    fragments: list[str] = []
    expected: list[int] = []
    line = 1
    for index in range(rng.randint(1, 7)):
        noise = _noise(rng)
        fragments.append(noise)
        line += noise.count("\n")
        expected.append(line)
        method = rng.choice(("unwrap", "expect"))
        arguments = '"oracle"' if method == "expect" else ""
        fragments.append(f'fn oracle_{index}() {{ value.{method}({arguments}); }}\n')
        line += 1
    fragments.append(_noise(rng))
    return _SourceCase("".join(fragments), expected)


def _scope_template(rng: random.Random) -> _ScopeFixture:
    depth = rng.randint(0, 12)
    noise = _noise(rng)
    prefix = '#[cfg(test)]\nfn only_test() {\n' + noise + "{\n" * depth
    test_line = prefix.count("\n") + 1
    closing = "}\n" * depth + "}\n"
    source = prefix + 'value.expect("test oracle");\n' + closing
    production_line = source.count("\n") + 1
    source += 'fn production() { value.unwrap(); }\n' + _noise(rng)
    return _ScopeFixture(source, test_line, production_line)


def _corrupt_scope(fixture: _ScopeFixture, closing: str) -> _SourceCase:
    """Change one known template boundary, never deriving an oracle from parsing."""
    lines = fixture.source.splitlines(keepends=True)
    if closing:
        lines.insert(fixture.production_line - 1, closing + "\n")
        production_line = fixture.production_line + 1
    else:
        del lines[fixture.production_line - 2]
        production_line = fixture.production_line - 1
    return _SourceCase("".join(lines), [fixture.test_line, production_line])


@dataclass(frozen=True)
class _Edge:
    owner: int
    target: int
    test_only: bool = field(kw_only=True)


@dataclass(frozen=True)
class _GraphObservation:
    enforced: frozenset[int]
    excluded: frozenset[int]
    scanned_files: int
    findings: int


def _edge_table(rng: random.Random, count: int) -> list[_Edge]:
    """A rooted tree ensures every file has an owner; extra edges add sharing/cycles."""
    tree = [
        _Edge(rng.randrange(node), node, test_only=bool(rng.randrange(2)))
        for node in range(1, count)
    ]
    extra = [
        _Edge(rng.randrange(count), rng.randrange(1, count), test_only=bool(rng.randrange(2)))
        for _ in range(rng.randrange(count * 2))
    ]
    return tree + extra


def _active_edges(edges: list[_Edge], *, tests_enabled: bool) -> list[_Edge]:
    return [edge for edge in edges if tests_enabled or not edge.test_only]


def _adjacency(count: int, edges: list[_Edge]) -> list[list[bool]]:
    matrix = [[left == right for right in range(count)] for left in range(count)]
    for edge in edges:
        matrix[edge.owner][edge.target] = True
    return matrix


def _through(matrix: list[list[bool]], pivot: int) -> list[list[bool]]:
    """One Boolean Floyd-Warshall step, independent of scanner ownership traversal."""
    return [
        [row[target] or (row[pivot] and matrix[pivot][target]) for target in range(len(matrix))]
        for row in matrix
    ]


def _oracle_reached(count: int, edges: list[_Edge], *, tests_enabled: bool) -> set[int]:
    matrix = _adjacency(count, _active_edges(edges, tests_enabled=tests_enabled))
    for pivot in range(count):
        matrix = _through(matrix, pivot)
    return {node for node in range(count) if matrix[0][node]}


def _node_name(node: int) -> str:
    return "lib.rs" if node == 0 else f"node_{node}.rs"


def _declaration(index: int, edge: _Edge) -> str:
    guard = "#[cfg(test)]\n" if edge.test_only else ""
    return guard + f'#[path = "{_node_name(edge.target)}"] mod edge_{index};\n'


def _write_graph(
    root: Path, count: int, edges: list[_Edge], *, root_declaration: str
) -> list[Path]:
    files: list[Path] = []
    declarations: list[list[str]] = [[] for _ in range(count)]
    declarations[0].append(root_declaration)
    for index, edge in enumerate(edges):
        declarations[edge.owner].append(_declaration(index, edge))
    for node in range(count):
        path = root / "src" / _node_name(node)
        path.parent.mkdir(parents=True, exist_ok=True)
        source = 'fn oracle_probe() { value.expect("oracle"); }\n'
        path.write_text(source + "".join(declarations[node]), encoding="utf-8")
        files.append(path)
    return files


def _observed_graph(
    count: int, edges: list[_Edge], *, root_declaration: str = ""
) -> _GraphObservation:
    with _repository() as root:
        files = _write_graph(root, count, edges, root_declaration=root_declaration)
        result = CHECKER.scan_files(files, [])
        findings = {path for path, _, _ in result.findings}
        excluded = CHECKER.test_only_files(files)
        enforced_nodes = {node for node in range(count) if f"src/{_node_name(node)}" in findings}
        excluded_nodes = {node for node, path in enumerate(files) if path.resolve() in excluded}
        return _GraphObservation(
            frozenset(enforced_nodes), frozenset(excluded_nodes),
            result.scanned_files, len(result.findings),
        )


def _expected_graph(production: set[int], all_reached: set[int]) -> _GraphObservation:
    return _GraphObservation(
        frozenset(production), frozenset(all_reached - production),
        len(production), len(production),
    )


class GeneratedScannerProperties(unittest.TestCase):
    def test_inert_noise_preserves_independently_marked_production_calls(self) -> None:
        """No generated comment/string bait creates or hides a template call."""
        rng = random.Random(_NOISE_SEED)
        for case in range(96):
            with self.subTest(seed=_NOISE_SEED, case=case):
                sample = _production_template(rng)
                self.assertEqual(_scan_source(sample.source), sample.expected_lines, sample.source)

    def test_balanced_scope_excludes_only_tests_and_malformed_scope_fails_closed(self) -> None:
        """A mismatched delimiter revokes exclusion instead of hiding adjacent code."""
        rng = random.Random(_SCOPE_SEED)
        for case in range(96):
            with self.subTest(seed=_SCOPE_SEED, case=case):
                self._assert_scope_case(rng)

    def _assert_scope_case(self, rng: random.Random) -> None:
        fixture = _scope_template(rng)
        self.assertEqual(_scan_source(fixture.source), [fixture.production_line], fixture.source)
        for closing in ("}", "]", ")", ""):
            sample = _corrupt_scope(fixture, closing)
            self.assertEqual(_scan_source(sample.source), sample.expected_lines, sample.source)

    def test_generated_graphs_match_independent_compile_environment_reachability(self) -> None:
        """Any production route wins, including after adding a new shared owner."""
        rng = random.Random(_GRAPH_SEED)
        for case in range(96):
            count = rng.randint(2, 8)
            edges = _edge_table(rng, count)
            with self.subTest(seed=_GRAPH_SEED, case=case, edges=edges):
                self._assert_graph_case(rng, count, edges)

    def _assert_graph_case(self, rng: random.Random, count: int, edges: list[_Edge]) -> None:
        production = _oracle_reached(count, edges, tests_enabled=False)
        all_reached = _oracle_reached(count, edges, tests_enabled=True)
        self.assertEqual(_observed_graph(count, edges), _expected_graph(production, all_reached))
        shared_candidates = (all_reached - production) or (all_reached - {0})
        target = rng.choice(sorted(shared_candidates))
        shared = edges + [_Edge(0, target, test_only=False)]
        after = _oracle_reached(count, shared, tests_enabled=False)
        self.assertEqual(_observed_graph(count, shared), _expected_graph(after, all_reached))
        self.assertLessEqual(production, after)
        self.assertIn(target, after)
        self._assert_ambiguous_cases(rng, count, edges)

    def _assert_ambiguous_cases(self, rng: random.Random, count: int, edges: list[_Edge]) -> None:
        all_reached = _oracle_reached(count, edges, tests_enabled=True)
        target = rng.randrange(1, count)
        ambiguous = (
            f'#[cfg_attr(feature = "extra", path = "{_node_name(target)}")] '
            'mod ambiguous_owner;\n'
        )
        # A possible production root is an unconditional edge in the independent
        # compile-environment model; its non-test descendants must also be kept.
        possible = edges + [_Edge(0, target, test_only=False)]
        production = _oracle_reached(count, possible, tests_enabled=False)
        self.assertEqual(
            _observed_graph(count, edges, root_declaration=ambiguous),
            _expected_graph(production, all_reached), ambiguous,
        )
        unresolved = (
            '#[cfg(test)]\n'
            f'#[path = concat!("node_", "{target}.rs")] mod computed_owner;\n'
        )
        # No filename is provable here: the declared policy keeps every tracked
        # target, independently of all ordinary graph reachability relations.
        self.assertEqual(
            _observed_graph(count, edges, root_declaration=unresolved),
            _expected_graph(set(range(count)), all_reached), unresolved,
        )
