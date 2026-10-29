"""Balanced Rust source contexts without assuming arbitrary macros are tests."""

from __future__ import annotations

from pathlib import Path
from types import MappingProxyType

from .attributes import AttributeGroup, ModuleDeclaration, RustAttribute, SourceSpan
from .items import AttributedItem
from .lexer import IDENTIFIER_RE, RustLexer


_OPENING = MappingProxyType({"(": ")", "[": "]", "{": "}"})


class RustSource:
    """Map Rust tokens, attributes, and proven local test scopes to source text.

    Parameters
    ----------
    source : str
        Complete source to analyse without macro expansion or feature evaluation.

    Attributes
    ----------
    tokens : list of RustToken
        Source-ordered tokens with character offsets.
    pairs : dict of int to int
        Opening-to-closing token indices, empty if any delimiter is malformed.
    test_spans : list of SourceSpan
        Only balanced, explicitly proven test scopes; adjacent code is retained.

    Notes
    -----
    Unknown conditions and malformed syntax do not establish an exclusion.
    Exact known test attributes are interpreted without trusting macro names.
    """

    def __init__(self, source: str) -> None:
        self.source = source
        self.tokens = RustLexer(source).tokens()
        self.pairs = self._delimiter_pairs()
        self.groups = self._attribute_groups()
        self.attributes = {group.next_index: group.attributes for group in self.groups}
        self.test_spans = self._collect_test_spans()
        self.module_attributes = self._collect_module_attributes()

    @classmethod
    def read(cls, path: Path) -> RustSource:
        """Read and analyse a UTF-8 source file using the scanner's lossy decoding.

        Parameters
        ----------
        path : pathlib.Path
            Existing Rust file; filesystem errors are propagated to the caller.

        Returns
        -------
        RustSource
            Independent source map for this file.

        Raises
        ------
        OSError
            If the file cannot be read.
        """
        return cls(path.read_text(encoding="utf-8", errors="ignore"))

    def text(self, index: int) -> str:
        """Return token spelling, or an empty sentinel beyond the final token.

        Parameters
        ----------
        index : int
            Non-negative token index. Character offsets are not accepted.
        """
        return self.tokens[index].text if index < len(self.tokens) else ""

    def _pair_token(self, index: int, stack: list[int], pairs: dict[int, int]) -> bool:
        token = self.tokens[index]
        if token.literal:
            return True
        if token.text in _OPENING:
            stack.append(index)
            return True
        if token.text not in _OPENING.values():
            return True
        if not stack:
            return False
        if _OPENING[self.text(stack[-1])] != token.text:
            return False
        pairs[stack.pop()] = index
        return True

    def _delimiter_pairs(self) -> dict[int, int]:
        pairs: dict[int, int] = {}
        stack: list[int] = []
        for index in range(len(self.tokens)):
            if not self._pair_token(index, stack, pairs):
                # Invalid syntax proves no scope is safe to exclude.
                return {}
        return {} if stack else pairs

    def _read_attribute(self, index: int, *, inner: bool) -> RustAttribute | None:
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

    def _attribute_group(self, start: int) -> AttributeGroup:
        index = start
        inner = self.text(start + 1) == "!"
        attributes: list[RustAttribute] = []
        while (attribute := self._read_attribute(index, inner=inner)) is not None:
            attributes.append(attribute)
            index = attribute.next_index
        return AttributeGroup(start, index, inner=inner, attributes=attributes)

    def _attribute_groups(self) -> list[AttributeGroup]:
        groups: list[AttributeGroup] = []
        indices = (index for index, token in enumerate(self.tokens) if token.text == "#")
        next_index = 0
        for index in indices:
            if index < next_index:
                continue
            group = self._attribute_group(index)
            groups.append(group)
            next_index = max(index + 1, group.next_index)
        return [group for group in groups if group.attributes]

    def _after_group(self, index: int) -> int:
        if self.text(index) not in _OPENING:
            return index + 1
        close = self.pairs.get(index)
        return len(self.tokens) if close is None else close + 1

    def _test_span(self, group: AttributeGroup) -> SourceSpan | None:
        if group.inner:
            return SourceSpan(0, self.tokens[-1].end) if group.start == 0 else None
        end = AttributedItem(self, group.next_index).end()
        if end is None or end <= group.next_index:
            return None
        return SourceSpan(self.tokens[group.start].start, self.tokens[end - 1].end)

    def _collect_test_spans(self) -> list[SourceSpan]:
        test_groups = (group for group in self.groups if group.proves_test())
        spans = (self._test_span(group) for group in test_groups)
        return [span for span in spans if span is not None]

    def _visible_item(self, index: int) -> int:
        if self.text(index) != "pub":
            return index
        index += 1
        return self._after_group(index) if self.text(index) == "(" else index

    def _collect_module_attributes(self) -> dict[int, list[RustAttribute]]:
        return {self._visible_item(index): attrs for index, attrs in self.attributes.items()}

    def is_test(self, offset: int) -> bool:
        """Return whether a character offset has an explicit local test proof.

        Notes
        -----
        External-module ownership is evaluated separately by ModuleGraph.
        """
        return any(span.contains(offset) for span in self.test_spans)

    def module_at(self, index: int) -> ModuleDeclaration | None:
        """Recognise a module declaration beginning at a token index.

        Returns
        -------
        ModuleDeclaration or None
            A module identifier/body position, or None for other token shapes.
            External path resolution and existence checks happen separately.
        """
        if self.text(index) != "mod":
            return None
        name = self.text(index + 1)
        if not IDENTIFIER_RE.fullmatch(name):
            return None
        return ModuleDeclaration(name, index, index + 2, self.module_attributes.get(index, []))

    def _include_path(self, index: int) -> str | None:
        words = [token.text for token in self.tokens[index : index + 3]]
        if words != ["include", "!", "("]:
            return None
        return self.tokens[index + 3].path_value()

    def include_paths(self) -> list[str]:
        """Return known plain literals passed to include! as possible code owners.

        Returns
        -------
        list of str
            Unescaped paths. These edges conservatively retain production use,
            even when another module includes the same file under cfg(test).
        """
        values = (self._include_path(index) for index in range(len(self.tokens) - 4))
        return [value for value in values if value is not None]

    def _is_unwrap_call(self, index: int) -> bool:
        if self.text(index) != ".":
            return False
        if self.text(index + 1) not in {"unwrap", "expect"}:
            return False
        return self.text(index + 2) == "("

    def production_call_lines(self) -> list[int]:
        """Report unwrap/expect calls without a proven local test scope.

        Returns
        -------
        list of int
            Sorted, unique one-based source lines containing the call's dot.
            Comment/literal bait is ignored; filename policy and allowlists are
            deliberately applied by the entry point rather than this source map.
        """
        calls = (index for index in range(len(self.tokens) - 2) if self._is_unwrap_call(index))
        offsets = (self.tokens[index].start for index in calls)
        production = (offset for offset in offsets if not self.is_test(offset))
        return sorted({self.source.count("\n", 0, offset) + 1 for offset in production})
