"""Bound attributed declarations separately from comma-delimited expressions.

Angle brackets are interpreted only after recognising a declaration. Treating
an expression's comparison operator as a generic bracket could otherwise hide
an adjacent production arm. Const-expression groups within generic headers are
skipped with the source's balanced delimiter map.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .syntax import RustSource


_MODIFIERS = frozenset({"async", "unsafe", "default", "auto"})
_DECLARATIONS = frozenset({
    "fn", "impl", "struct", "enum", "union", "trait", "mod", "static",
    "type", "use", "macro", "macro_rules",
})
_STATEMENTS = frozenset({"const", "static", "type", "use"})
_CLOSING = frozenset({"}", "]", ")"})


class AttributedItem:
    """Track one attributed item's header without parsing arbitrary Rust.

    Parameters
    ----------
    source : RustSource
        Token positions and balanced delimiter proofs for the whole file.
    start : int
        Index of the first token following the item's attributes.

    Notes
    -----
    Only declared item headers ignore commas. Unknown syntax retains expression
    boundaries, and unterminated generic headers do not establish an exclusion.
    """

    def __init__(self, source: RustSource, start: int) -> None:
        self.source = source
        self.index = start
        self.kind = self._declaration_kind(start)
        self.angle_depth = 0

    def end(self) -> int | None:
        """Return the exclusive token boundary of this item, if proven.

        Returns
        -------
        int or None
            Exclusive token index, or ``None`` for an unresolved header/body.
            This bound never depends on braces embedded in literal text.
        """
        while self.index < len(self.source.tokens) and not self._invalid_header():
            boundary = self._boundary()
            if boundary is not None:
                return boundary
            self._advance()
        return None

    def _skip_modifiers(self, index: int) -> int:
        while self.source.text(index) in _MODIFIERS:
            index += 1
        return index

    def _declaration_kind(self, start: int) -> str | None:
        index = self._skip_modifiers(self.source._visible_item(start))
        word = self.source.text(index)
        if word == "const":
            return self._const_kind(index + 1)
        if word == "extern":
            return self._extern_kind(index + 1)
        return word if word in _DECLARATIONS else None

    def _const_kind(self, index: int) -> str | None:
        following = self._skip_modifiers(index)
        if self.source.text(following) == "extern":
            return self._extern_kind(following + 1)
        if self.source.text(following) == "fn":
            return "fn"
        word = self.source.text(index)
        return "const" if word.isidentifier() else None

    def _extern_kind(self, index: int) -> str:
        following = self.source.tokens[index : index + 1]
        if following and following[0].literal:
            index += 1
        index = self._skip_modifiers(index)
        return "fn" if self.source.text(index) == "fn" else "extern"

    def _invalid_header(self) -> bool:
        return self.angle_depth > 0 and self.source.text(self.index) in _CLOSING | {";"}

    def _boundary(self) -> int | None:
        text = self.source.text(self.index)
        if text == ";":
            return self.index + 1
        if text == "," and self.kind is None:
            return self.index + 1
        if text in _CLOSING:
            return self.index
        if self._ends_on_block(text):
            close = self.source.pairs.get(self.index)
            return None if close is None else close + 1
        return None

    def _ends_on_block(self, text: str) -> bool:
        if text != "{" or self.angle_depth:
            return False
        return self.kind not in _STATEMENTS

    def _advance(self) -> None:
        text = self.source.text(self.index)
        if self._opens_angle(text):
            self.angle_depth += 1
        if self._closes_angle(text):
            self.angle_depth -= 1
        self.index = self.source._after_group(self.index)

    def _opens_angle(self, text: str) -> bool:
        if self.kind is None or self.kind in _STATEMENTS:
            return False
        return text == "<"

    def _closes_angle(self, text: str) -> bool:
        if text != ">" or self.angle_depth == 0:
            return False
        return self.source.text(self.index - 1) not in {"-", "="}
