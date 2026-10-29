"""Resolve conservative Rust module ownership, including shared production use."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from .attributes import ModuleDeclaration, RustAttribute
from .syntax import RustSource


@dataclass(frozen=True)
class ModuleReference:
    """One possible code owner of a canonical tracked Rust path.

    Attributes
    ----------
    target : pathlib.Path
        Canonical existing tracked target.
    test_only : bool
        Whether this edge has a local explicit test-only proof.
    ambiguous : bool
        Whether resolution must retain production enforcement regardless of owner.
    """

    target: Path
    test_only: bool = field(kw_only=True)
    ambiguous: bool = field(default=False, kw_only=True)


@dataclass(frozen=True)
class ModuleBody:
    """An inline module body in token-index coordinates.

    Attributes
    ----------
    opening_index, closing_index : int
        Balanced brace-token positions, not source character offsets.
    """

    opening_index: int
    closing_index: int


@dataclass(frozen=True)
class ModuleScope:
    """Directory and token-boundary context for an inline module traversal.

    Attributes
    ----------
    start, end : int
        Half-open token indices within one source file.
    directory, path_directory : pathlib.Path
        Bases for default module names and explicit path attributes respectively.
    uncertain : bool
        Whether an ambiguous ancestor invalidates a test-only ownership proof.
    """

    start: int
    end: int
    directory: Path
    path_directory: Path
    uncertain: bool = field(default=False, kw_only=True)

    @classmethod
    def root(cls, path: Path, source: RustSource) -> ModuleScope:
        """Create directory context using Rust's mod.rs/root versus file-module rules."""
        directory = path.parent if path.name in {"lib.rs", "main.rs", "mod.rs"} else path.with_suffix("")
        return cls(0, len(source.tokens), directory, path.parent)

    def child(self, body: ModuleBody, directory: Path, *, ambiguous: bool) -> ModuleScope:
        """Enter an inline body while retaining uncertainty from its ancestors."""
        return ModuleScope(
            body.opening_index + 1, body.closing_index, directory, directory,
            uncertain=self.uncertain or ambiguous,
        )


class ModulePaths:
    """Interpret module attributes without guessing escaped or computed paths.

    Parameters
    ----------
    attributes : list of RustAttribute
        Attributes associated with one module declaration.

    Attributes
    ----------
    explicit, possible : list of str
        Exact path assignments and conservative plain-literal possibilities.
    unknown, ambiguous : bool
        Whether the target is unknowable or has multiple possible code owners.
    """

    def __init__(self, attributes: list[RustAttribute]) -> None:
        self.explicit = self._explicit_paths(attributes)
        self.possible = [value for attr in attributes for value in attr.possible_paths()]
        self.unknown = any(attr.has_unknown_path() for attr in attributes)
        self.ambiguous = len(self.explicit) > 1 or self._has_ambiguous_path(attributes)

    @staticmethod
    def _explicit_paths(attributes: list[RustAttribute]) -> list[str]:
        values = (attr.path_value() for attr in attributes)
        return [value for value in values if value is not None]

    @staticmethod
    def _has_ambiguous_path(attributes: list[RustAttribute]) -> bool:
        mentions = (attr for attr in attributes if attr.mentions_path())
        return any(attr.path_value() is None for attr in mentions)

    def _inline_directories(self, scope: ModuleScope, name: str) -> list[Path]:
        return self._candidates(scope, [scope.directory / name])

    def _possible_paths(self, scope: ModuleScope) -> list[Path]:
        return [scope.path_directory / value for value in self.possible]

    def _external_candidates(self, scope: ModuleScope, name: str) -> list[Path]:
        defaults = [scope.directory / (name + ".rs"), scope.directory / name / "mod.rs"]
        return self._candidates(scope, defaults)

    def _candidates(self, scope: ModuleScope, defaults: list[Path]) -> list[Path]:
        if self.ambiguous:
            return defaults + self._possible_paths(scope)
        if self.explicit:
            return [scope.path_directory / self.explicit[0]]
        return defaults


class ModuleResolver:
    """Resolve balanced module declarations and includes from one tracked file.

    Parameters
    ----------
    path : pathlib.Path
        Canonical source path.
    files : set of pathlib.Path
        Canonical tracked Rust files eligible as external module targets.

    Notes
    -----
    Unknown path expressions keep every possible tracked target enforced.
    Filesystem errors are propagated rather than silently omitting ownership.
    """

    def __init__(self, path: Path, files: set[Path]) -> None:
        self.path = path
        self.files = files
        self.source = RustSource.read(path)
        self.references: list[ModuleReference] = []

    def resolve(self) -> list[ModuleReference]:
        """Return discovered external owners without discarding uncertain references.

        Returns
        -------
        list of ModuleReference
            Canonical tracked targets; ambiguous edges are explicitly marked.
        """
        self._visit(ModuleScope.root(self.path, self.source))
        self._visit_includes()
        return self.references

    def _visit(self, scope: ModuleScope) -> None:
        index = scope.start
        while index + 2 < scope.end:
            declaration = self.source.module_at(index)
            index = index + 1 if declaration is None else self._visit_module(scope, declaration)

    def _visit_module(self, scope: ModuleScope, declaration: ModuleDeclaration) -> int:
        paths = ModulePaths(declaration.attributes)
        if paths.unknown:
            # An unresolved path could name any tracked file; retain enforcement.
            self.references.extend(ModuleReference(target, test_only=False, ambiguous=True) for target in self.files)
        delimiter = self.source.text(declaration.body_index)
        if delimiter == "{":
            return self._visit_inline(scope, declaration, paths)
        if delimiter == ";":
            self._visit_external(scope, declaration, paths)
        return declaration.index + 1

    def _visit_inline(self, scope: ModuleScope, declaration: ModuleDeclaration, paths: ModulePaths) -> int:
        close = self.source.pairs.get(declaration.body_index)
        if close is None:
            return declaration.index + 1
        body = ModuleBody(declaration.body_index, close)
        for child in paths._inline_directories(scope, declaration.name):
            self._visit(scope.child(body, child, ambiguous=paths.ambiguous))
        return close + 1

    def _visit_external(self, scope: ModuleScope, declaration: ModuleDeclaration, paths: ModulePaths) -> None:
        candidates = paths._external_candidates(scope, declaration.name)
        resolved = {path.resolve() for path in candidates if path.resolve() in self.files}
        ambiguous = scope.uncertain or paths.ambiguous or len(resolved) != 1
        test_only = not ambiguous and self.source.is_test(self.source.tokens[declaration.index].start)
        self.references.extend(ModuleReference(target, test_only=test_only, ambiguous=ambiguous) for target in resolved)

    def _visit_includes(self) -> None:
        for value in self.source.include_paths():
            target = (self.path.parent / value).resolve()
            if target in self.files:
                self.references.append(ModuleReference(target, test_only=False, ambiguous=True))


class ModuleGraph:
    """Compute inherited test ownership while allowing production routes to win.

    Parameters
    ----------
    files : list of pathlib.Path
        Tracked Rust files; paths and results are canonicalised with resolve().
    is_test_path : callable
        Keyword-only repository filename policy. It is supplied at call time so
        the entry point's current repository root remains authoritative.

    Notes
    -----
    Unowned files and conventional crate roots are production roots. A possible
    production owner, include!, or unresolved path defeats a test-only proof.
    """

    def __init__(self, files: list[Path], *, is_test_path: Callable[[Path], bool]) -> None:
        self.is_test_path = is_test_path
        self.files = {path.resolve() for path in files}
        self.references = {path: ModuleResolver(path, self.files).resolve() for path in self.files}
        self.edges = [edge for refs in self.references.values() for edge in refs]

    def _descendants(self, seeds: set[Path], *, production_only: bool = False) -> set[Path]:
        reached = set(seeds)
        pending = list(seeds)
        while pending:
            added = self._next_targets(pending.pop(), reached, production_only=production_only)
            reached.update(added)
            pending.extend(added)
        return reached

    def _next_targets(self, parent: Path, reached: set[Path], *, production_only: bool) -> set[Path]:
        edges = self.references[parent]
        if production_only:
            edges = [edge for edge in edges if not edge.test_only]
        return {edge.target for edge in edges} - reached

    def _test_seeds(self) -> set[Path]:
        seeds = {edge.target for edge in self.edges if edge.test_only}
        seeds.update(path for path in self.files if self.is_test_path(path))
        return seeds

    def _production_roots(self, candidates: set[Path]) -> set[Path]:
        incoming = {edge.target for edge in self.edges}
        production = self.files - candidates
        production.update(self.files - incoming)
        production.update(edge.target for edge in self.edges if edge.ambiguous)
        production.update(path for path in self.files if path.name in {"lib.rs", "main.rs", "build.rs"})
        return production

    def test_only(self) -> set[Path]:
        """Return only files with test ownership and no possible production owner.

        Returns
        -------
        set of pathlib.Path
            Canonical paths safe to exclude in addition to filename policy.
            Shared production/test targets always remain enforced.
        """
        candidates = self._descendants(self._test_seeds())
        production = self._production_roots(candidates)
        return candidates - self._descendants(production, production_only=True)
