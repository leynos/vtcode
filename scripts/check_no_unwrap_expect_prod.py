#!/usr/bin/env python3
"""Warn or enforce against unwrap/expect usage in non-test Rust code."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from fnmatch import fnmatch
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
UNWRAP_EXPECT_RE = re.compile(r"\.(unwrap|expect)\s*\(")
TEST_ROOT_DIRECTORIES = ("fuzz", "target", "tests")
NESTED_TEST_DIRECTORIES = ("tests", "benches", "examples")
TEST_SOURCE_SUFFIXES = ("_test.rs", "_tests.rs")
RAW_STRING_RE = re.compile(r'(?:br|cr|r)(#*)"')
QUOTED_RE = re.compile(
    r"(?:b|c)?\"|(?:b)?'(?:[^\\'\n]|\\(?:[^\n]|u\{[0-9a-fA-F_]+\}))'"
)
IDENTIFIER_RE = re.compile(r"[a-zA-Z_][a-zA-Z_0-9]*")


@dataclass
class AllowRule:
    pattern: str
    line: int | None = None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate production Rust code does not use unwrap()/expect().",
    )
    parser.add_argument(
        "--mode",
        choices=("warn", "enforce"),
        default="warn",
        help="warn: report only; enforce: fail on findings.",
    )
    parser.add_argument(
        "--allowlist",
        type=Path,
        default=Path("scripts/zen_allowlist.txt"),
        help="Allowlist file with path or path:line patterns (default: scripts/zen_allowlist.txt).",
    )
    parser.add_argument(
        "--max-findings",
        type=int,
        default=200,
        help="Maximum number of findings to print (default: 200).",
    )
    return parser.parse_args()


def list_tracked_rust_files() -> list[Path]:
    proc = subprocess.run(
        ["git", "ls-files", "-z", "*.rs"],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
    )
    raw = proc.stdout.decode("utf-8", errors="ignore")
    return [REPO_ROOT / Path(rel) for rel in raw.split("\0") if rel]


def is_test_path(path: Path) -> bool:
    rel = path.relative_to(REPO_ROOT).as_posix()
    path_parts = rel.split("/")
    return (
        _is_test_tree_path(path_parts)
        or _is_test_module_path(path_parts)
        or _is_test_source_path(path_parts[-1])
    )


def _is_test_tree_path(path_parts: list[str]) -> bool:
    has_workspace_test_root = (
        len(path_parts) > 1 and path_parts[0] in TEST_ROOT_DIRECTORIES
    )
    has_nested_test_directory = any(
        directory in NESTED_TEST_DIRECTORIES for directory in path_parts[1:-1]
    )
    return has_workspace_test_root or has_nested_test_directory


def _is_test_module_path(path_parts: list[str]) -> bool:
    return any(part.endswith("_tests") for part in path_parts[:-1])


def _is_test_source_path(filename: str) -> bool:
    return filename == "tests.rs" or filename.endswith(TEST_SOURCE_SUFFIXES)


def parse_allow_rules(path: Path) -> list[AllowRule]:
    if not path.is_absolute():
        path = REPO_ROOT / path
    if not path.exists():
        return []

    rules: list[AllowRule] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue

        token = line.split("|", 1)[0].strip()
        if not token:
            continue

        if ":" in token:
            prefix, _, suffix = token.rpartition(":")
            if suffix.isdigit() and prefix:
                rules.append(AllowRule(pattern=prefix, line=int(suffix)))
                continue

        rules.append(AllowRule(pattern=token, line=None))

    return rules


def is_allowlisted(rel_path: str, line_no: int, rules: list[AllowRule]) -> bool:
    # The allowlist was authored against the pre-restructure short form
    # (e.g. "vtcode-core/src/..."); match both it and the current
    # "crates/codegen|common/..." form so entries do not go stale when the
    # workspace layout changes.
    candidates = {rel_path}
    if rel_path.startswith("crates/"):
        parts = rel_path.split("/", 2)
        if len(parts) > 2:
            candidates.add(parts[2])
    for rule in rules:
        for candidate in candidates:
            if not fnmatch(candidate, rule.pattern):
                continue
            if rule.line is None or rule.line == line_no:
                return True
    return False


@dataclass(frozen=True)
class RustToken:
    text: str
    start: int
    end: int
    literal: bool = False

    def path_value(self) -> str | None:
        if not self.literal or not self.text.startswith('"'):
            return None
        return None if "\\" in self.text else self.text[1:-1]


class RustLexer:
    """Own the lexical cursor and preserve offsets for source diagnostics."""

    def __init__(self, source: str) -> None:
        self.source = source
        self.offset = 0

    def tokens(self) -> list[RustToken]:
        tokens: list[RustToken] = []
        while self.offset < len(self.source):
            if not self.skip_ignored():
                tokens.append(self.read_token())
        return tokens

    def skip_ignored(self) -> bool:
        if self.source[self.offset].isspace():
            self.offset += 1
            return True
        if self.source.startswith("//", self.offset):
            end = self.source.find("\n", self.offset)
            self.offset = len(self.source) if end < 0 else end
            return True
        if self.source.startswith("/*", self.offset):
            self.skip_block_comment()
            return True
        return False

    def skip_block_comment(self) -> None:
        depth = 1
        self.offset += 2
        changes = {"/*": 1, "*/": -1}
        while depth and self.offset < len(self.source):
            pair = self.source[self.offset : self.offset + 2]
            depth += changes.get(pair, 0)
            self.offset += 2 if pair in changes else 1

    def take(self, end: int, literal: bool = False) -> RustToken:
        token = RustToken(self.source[self.offset:end], self.offset, end, literal)
        self.offset = end
        return token

    def read_token(self) -> RustToken:
        raw = RAW_STRING_RE.match(self.source, self.offset)
        if raw:
            return self.read_raw(raw)
        quoted = QUOTED_RE.match(self.source, self.offset)
        if quoted:
            return self.read_quoted(quoted)
        word = IDENTIFIER_RE.match(self.source, self.offset)
        return self.take(word.end() if word else self.offset + 1)

    def read_raw(self, match: re.Match[str]) -> RustToken:
        marker = '"' + match.group(1)
        close = self.source.find(marker, match.end())
        end = len(self.source) if close < 0 else close + len(marker)
        return self.take(end, literal=True)

    def read_quoted(self, match: re.Match[str]) -> RustToken:
        end = match.end()
        if self.source[end - 1] != "'":
            end = self.string_end(end)
        return self.take(end, literal=True)

    def string_end(self, offset: int) -> int:
        while offset < len(self.source):
            char = self.source[offset]
            if char == '"':
                return offset + 1
            offset += 2 if char == "\\" else 1
        return offset


@dataclass(frozen=True)
class SourceSpan:
    start: int
    end: int

    def contains(self, offset: int) -> bool:
        return self.start <= offset < self.end


@dataclass(frozen=True)
class RustAttribute:
    tokens: list[RustToken]
    next_index: int

    def words(self) -> list[str]:
        return [token.text for token in self.tokens]

    def proves_test(self) -> bool:
        """Unknown user macros cannot establish test-only compilation."""
        words = self.words()
        if words in (["cfg", "(", "test", ")"], ["test"]):
            return True
        if words[:4] != ["tokio", ":", ":", "test"]:
            return False
        return len(words) == 4 or (words[4] == "(" and words[-1] == ")")

    def path_value(self) -> str | None:
        if len(self.tokens) != 3:
            return None
        if self.words()[:2] != ["path", "="]:
            return None
        return self.tokens[2].path_value()

    def mentions_path(self) -> bool:
        return "path" in self.words()

    def unknown_path_at(self, index: int) -> bool:
        if self.tokens[index].text != "path":
            return False
        assignment = self.tokens[index + 1 : index + 3]
        if len(assignment) != 2:
            return True
        return assignment[0].text != "=" or assignment[1].path_value() is None

    def has_unknown_path(self) -> bool:
        return any(self.unknown_path_at(index) for index in range(len(self.tokens)))

    def possible_paths(self) -> list[str]:
        values = (token.path_value() for token in self.tokens)
        return [value for value in values if value is not None]


@dataclass(frozen=True)
class AttributeGroup:
    start: int
    next_index: int
    inner: bool
    attributes: list[RustAttribute]

    def proves_test(self) -> bool:
        return any(attribute.proves_test() for attribute in self.attributes)


@dataclass(frozen=True)
class ModuleDeclaration:
    name: str
    index: int
    body_index: int
    attributes: list[RustAttribute]


class RustSource:
    """Map tokens, attributes, and balanced local test scopes to source offsets."""

    OPENING = {"(": ")", "[": "]", "{": "}"}

    def __init__(self, source: str) -> None:
        self.source = source
        self.tokens = RustLexer(source).tokens()
        self.pairs = self.delimiter_pairs()
        self.groups = self.attribute_groups()
        self.attributes = {group.next_index: group.attributes for group in self.groups}
        self.test_spans = self.collect_test_spans()
        self.module_attributes = self.collect_module_attributes()

    @classmethod
    def read(cls, path: Path) -> RustSource:
        return cls(path.read_text(encoding="utf-8", errors="ignore"))

    def text(self, index: int) -> str:
        return self.tokens[index].text if index < len(self.tokens) else ""

    def pair_token(self, index: int, stack: list[int], pairs: dict[int, int]) -> bool:
        token = self.tokens[index]
        if token.literal:
            return True
        if token.text in self.OPENING:
            stack.append(index)
            return True
        if token.text not in self.OPENING.values():
            return True
        if not stack:
            return False
        if self.OPENING[self.text(stack[-1])] != token.text:
            return False
        pairs[stack.pop()] = index
        return True

    def delimiter_pairs(self) -> dict[int, int]:
        pairs: dict[int, int] = {}
        stack: list[int] = []
        for index in range(len(self.tokens)):
            if not self.pair_token(index, stack, pairs):
                # Invalid syntax proves no scope is safe to exclude.
                return {}
        return {} if stack else pairs

    def read_attribute(self, index: int, inner: bool) -> RustAttribute | None:
        if self.text(index) != "#":
            return None
        if (self.text(index + 1) == "!") != inner:
            return None
        bracket = index + 2 if inner else index + 1
        if self.text(bracket) != "[":
            return None
        close = self.pairs.get(bracket)
        if close is None:
            return None
        return RustAttribute(self.tokens[bracket + 1 : close], close + 1)

    def attribute_group(self, start: int) -> AttributeGroup:
        index = start
        inner = self.text(start + 1) == "!"
        attributes: list[RustAttribute] = []
        while (attribute := self.read_attribute(index, inner)) is not None:
            attributes.append(attribute)
            index = attribute.next_index
        return AttributeGroup(start, index, inner, attributes)

    def attribute_groups(self) -> list[AttributeGroup]:
        groups: list[AttributeGroup] = []
        indices = (index for index, token in enumerate(self.tokens) if token.text == "#")
        next_index = 0
        for index in indices:
            if index < next_index:
                continue
            group = self.attribute_group(index)
            groups.append(group)
            next_index = max(index + 1, group.next_index)
        return [group for group in groups if group.attributes]

    def after_group(self, index: int) -> int:
        if self.text(index) not in self.OPENING:
            return index + 1
        close = self.pairs.get(index)
        return len(self.tokens) if close is None else close + 1

    def item_end(self, index: int) -> int | None:
        while index < len(self.tokens):
            boundary = self.item_boundary(index)
            if boundary is not None:
                return boundary
            index = self.after_group(index)
        return None

    def item_boundary(self, index: int) -> int | None:
        text = self.text(index)
        if text in {";", ","}:
            return index + 1
        if text in self.OPENING.values():
            return index
        if text == "{":
            close = self.pairs.get(index)
            return None if close is None else close + 1
        return None

    def test_span(self, group: AttributeGroup) -> SourceSpan | None:
        if group.inner:
            return SourceSpan(0, self.tokens[-1].end) if group.start == 0 else None
        end = self.item_end(group.next_index)
        if end is None or end <= group.next_index:
            return None
        return SourceSpan(self.tokens[group.start].start, self.tokens[end - 1].end)

    def collect_test_spans(self) -> list[SourceSpan]:
        test_groups = (group for group in self.groups if group.proves_test())
        spans = (self.test_span(group) for group in test_groups)
        return [span for span in spans if span is not None]

    def visible_item(self, index: int) -> int:
        if self.text(index) != "pub":
            return index
        index += 1
        return self.after_group(index) if self.text(index) == "(" else index

    def collect_module_attributes(self) -> dict[int, list[RustAttribute]]:
        return {self.visible_item(index): attrs for index, attrs in self.attributes.items()}

    def is_test(self, offset: int) -> bool:
        return any(span.contains(offset) for span in self.test_spans)

    def module_at(self, index: int) -> ModuleDeclaration | None:
        if self.text(index) != "mod":
            return None
        name = self.text(index + 1)
        if not IDENTIFIER_RE.fullmatch(name):
            return None
        return ModuleDeclaration(name, index, index + 2, self.module_attributes.get(index, []))

    def include_path(self, index: int) -> str | None:
        words = [token.text for token in self.tokens[index : index + 3]]
        if words != ["include", "!", "("]:
            return None
        return self.tokens[index + 3].path_value()

    def include_paths(self) -> list[str]:
        values = (self.include_path(index) for index in range(len(self.tokens) - 4))
        return [value for value in values if value is not None]

    def is_unwrap_call(self, index: int) -> bool:
        if self.text(index) != ".":
            return False
        if self.text(index + 1) not in {"unwrap", "expect"}:
            return False
        return self.text(index + 2) == "("

    def production_call_lines(self) -> list[int]:
        calls = (index for index in range(len(self.tokens) - 2) if self.is_unwrap_call(index))
        offsets = (self.tokens[index].start for index in calls)
        production = (offset for offset in offsets if not self.is_test(offset))
        return sorted({self.source.count("\n", 0, offset) + 1 for offset in production})


@dataclass(frozen=True)
class ModuleReference:
    target: Path
    test_only: bool
    ambiguous: bool = False


@dataclass(frozen=True)
class ModuleBody:
    opening_index: int
    closing_index: int


@dataclass(frozen=True)
class ModuleScope:
    start: int
    end: int
    directory: Path
    path_directory: Path
    uncertain: bool = False

    @classmethod
    def root(cls, path: Path, source: RustSource) -> ModuleScope:
        directory = path.parent if path.name in {"lib.rs", "main.rs", "mod.rs"} else path.with_suffix("")
        return cls(0, len(source.tokens), directory, path.parent)

    def child(self, body: ModuleBody, directory: Path, ambiguous: bool) -> ModuleScope:
        return ModuleScope(
            body.opening_index + 1, body.closing_index, directory, directory,
            self.uncertain or ambiguous,
        )


class ModulePaths:
    """Interpret only literal path proofs; retain every uncertain code owner."""

    def __init__(self, attributes: list[RustAttribute]) -> None:
        self.explicit = self.explicit_paths(attributes)
        self.possible = [value for attr in attributes for value in attr.possible_paths()]
        self.unknown = any(attr.has_unknown_path() for attr in attributes)
        self.ambiguous = len(self.explicit) > 1 or self.has_ambiguous_path(attributes)

    @staticmethod
    def explicit_paths(attributes: list[RustAttribute]) -> list[str]:
        values = (attr.path_value() for attr in attributes)
        return [value for value in values if value is not None]

    @staticmethod
    def has_ambiguous_path(attributes: list[RustAttribute]) -> bool:
        mentions = (attr for attr in attributes if attr.mentions_path())
        return any(attr.path_value() is None for attr in mentions)

    def inline_directories(self, scope: ModuleScope, name: str) -> list[Path]:
        return self.candidates(scope, [scope.directory / name])

    def possible_paths(self, scope: ModuleScope) -> list[Path]:
        return [scope.path_directory / value for value in self.possible]

    def external_candidates(self, scope: ModuleScope, name: str) -> list[Path]:
        defaults = [scope.directory / (name + ".rs"), scope.directory / name / "mod.rs"]
        return self.candidates(scope, defaults)

    def candidates(self, scope: ModuleScope, defaults: list[Path]) -> list[Path]:
        if self.ambiguous:
            return defaults + self.possible_paths(scope)
        if self.explicit:
            return [scope.path_directory / self.explicit[0]]
        return defaults


class ModuleResolver:
    """Resolve one source file's balanced module declarations and code includes."""

    def __init__(self, path: Path, files: set[Path]) -> None:
        self.path = path
        self.files = files
        self.source = RustSource.read(path)
        self.references: list[ModuleReference] = []

    def resolve(self) -> list[ModuleReference]:
        self.visit(ModuleScope.root(self.path, self.source))
        self.visit_includes()
        return self.references

    def visit(self, scope: ModuleScope) -> None:
        index = scope.start
        while index + 2 < scope.end:
            declaration = self.source.module_at(index)
            index = index + 1 if declaration is None else self.visit_module(scope, declaration)

    def visit_module(self, scope: ModuleScope, declaration: ModuleDeclaration) -> int:
        paths = ModulePaths(declaration.attributes)
        if paths.unknown:
            # An unresolved path could name any tracked file; retain enforcement.
            self.references.extend(ModuleReference(target, False, True) for target in self.files)
        delimiter = self.source.text(declaration.body_index)
        if delimiter == "{":
            return self.visit_inline(scope, declaration, paths)
        if delimiter == ";":
            self.visit_external(scope, declaration, paths)
        return declaration.index + 1

    def visit_inline(self, scope: ModuleScope, declaration: ModuleDeclaration, paths: ModulePaths) -> int:
        close = self.source.pairs.get(declaration.body_index)
        if close is None:
            return declaration.index + 1
        body = ModuleBody(declaration.body_index, close)
        for child in paths.inline_directories(scope, declaration.name):
            self.visit(scope.child(body, child, paths.ambiguous))
        return close + 1

    def visit_external(self, scope: ModuleScope, declaration: ModuleDeclaration, paths: ModulePaths) -> None:
        candidates = paths.external_candidates(scope, declaration.name)
        resolved = {path.resolve() for path in candidates if path.resolve() in self.files}
        ambiguous = scope.uncertain or paths.ambiguous or len(resolved) != 1
        test_only = not ambiguous and self.source.is_test(self.source.tokens[declaration.index].start)
        self.references.extend(ModuleReference(target, test_only, ambiguous) for target in resolved)

    def visit_includes(self) -> None:
        for value in self.source.include_paths():
            target = (self.path.parent / value).resolve()
            if target in self.files:
                self.references.append(ModuleReference(target, False, True))


class ModuleGraph:
    """Propagate ownership: one possible production owner defeats test exclusion."""

    def __init__(self, files: list[Path]) -> None:
        self.files = {path.resolve() for path in files}
        self.references = {path: ModuleResolver(path, self.files).resolve() for path in self.files}
        self.edges = [edge for refs in self.references.values() for edge in refs]

    def descendants(self, seeds: set[Path], production_only: bool = False) -> set[Path]:
        reached = set(seeds)
        pending = list(seeds)
        while pending:
            added = self.next_targets(pending.pop(), reached, production_only)
            reached.update(added)
            pending.extend(added)
        return reached

    def next_targets(self, parent: Path, reached: set[Path], production_only: bool) -> set[Path]:
        edges = self.references[parent]
        if production_only:
            edges = [edge for edge in edges if not edge.test_only]
        return {edge.target for edge in edges} - reached

    def test_seeds(self) -> set[Path]:
        seeds = {edge.target for edge in self.edges if edge.test_only}
        seeds.update(path for path in self.files if is_test_path(path))
        return seeds

    def production_roots(self, candidates: set[Path]) -> set[Path]:
        incoming = {edge.target for edge in self.edges}
        production = self.files - candidates
        production.update(self.files - incoming)
        production.update(edge.target for edge in self.edges if edge.ambiguous)
        production.update(path for path in self.files if path.name in {"lib.rs", "main.rs", "build.rs"})
        return production

    def test_only(self) -> set[Path]:
        candidates = self.descendants(self.test_seeds())
        production = self.production_roots(candidates)
        return candidates - self.descendants(production, production_only=True)


def test_only_files(files: list[Path]) -> set[Path]:
    return ModuleGraph(files).test_only()


def scan_file(path: Path, rules: list[AllowRule]) -> list[tuple[str, int, str]]:
    rel_path = path.relative_to(REPO_ROOT).as_posix()
    source = RustSource.read(path)
    lines = source.source.splitlines()
    findings: list[tuple[str, int, str]] = []
    for line_no in source.production_call_lines():
        if not is_allowlisted(rel_path, line_no, rules):
            findings.append((rel_path, line_no, lines[line_no - 1].strip()))
    return findings


@dataclass(frozen=True)
class ScanResult:
    scanned_files: int
    findings: list[tuple[str, int, str]]


def scan_files(files: list[Path], rules: list[AllowRule]) -> ScanResult:
    excluded = test_only_files(files)
    production = [path for path in files if not is_test_path(path)]
    production = [path for path in production if path.resolve() not in excluded]
    findings = [finding for path in production for finding in scan_file(path, rules)]
    return ScanResult(len(production), findings)


def report_findings(findings: list[tuple[str, int, str]], limit: int) -> None:
    if not findings:
        return
    print("\nFindings (path:line | source):")
    for rel_path, line_no, line_source in findings[:limit]:
        print(f"- {rel_path}:{line_no} | {line_source}")
    if len(findings) > limit:
        print(f"- ... {len(findings) - limit} additional finding(s) not shown")


def report_policy(mode: str) -> int:
    if mode == "enforce":
        print(
            "\nunwrap/expect enforcement failed.\n\n"
            "Remediation:\n"
            "1. Replace unwrap()/expect() with anyhow::Result + with_context() in production code.\n"
            "2. If an exception is intentional, add a scoped entry to scripts/zen_allowlist.txt with rationale.\n"
            "3. Re-run: python3 scripts/check_no_unwrap_expect_prod.py --mode enforce --allowlist scripts/zen_allowlist.txt",
            file=sys.stderr,
        )
        return 1
    print(
        "\nWarning mode: findings reported but not blocking. "
        "Promote to --mode enforce after cleanup/allowlisting."
    )
    return 0


def main() -> int:
    args = parse_args()
    result = scan_files(list_tracked_rust_files(), parse_allow_rules(args.allowlist))
    print(
        f"unwrap/expect scan summary: scanned_files={result.scanned_files}, "
        f"findings={len(result.findings)}, mode={args.mode}"
    )
    report_findings(result.findings, args.max_findings)
    return report_policy(args.mode) if result.findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
