"""Source spans and explicit Rust test/path attribute proofs."""

from __future__ import annotations

from dataclasses import dataclass, field

from .lexer import RustToken


@dataclass(frozen=True)
class SourceSpan:
    """A half-open span of source character offsets.

    Attributes
    ----------
    start, end : int
        Start-inclusive and end-exclusive positions in the original text.
    """

    start: int
    end: int

    def contains(self, offset: int) -> bool:
        """Return whether a character offset belongs to this half-open span.

        Parameters
        ----------
        offset : int
            Character position in the same original source as this span.
        """
        return self.start <= offset < self.end


@dataclass(frozen=True)
class RustAttribute:
    """One balanced Rust attribute and its following token position.

    Parameters
    ----------
    tokens : list of RustToken
        Attribute contents, excluding the outer brackets.
    next_index : int
        Token index immediately after the attribute's closing bracket.
    """

    tokens: list[RustToken]
    next_index: int

    def words(self) -> list[str]:
        """Return literal token spellings without reconstructing source spacing."""
        return [token.text for token in self.tokens]

    def proves_test(self) -> bool:
        """Decide whether this exact attribute proves test-only compilation.

        Returns
        -------
        bool
            True only for cfg(test), built-in test, or the supported Tokio test
            macro. Mixed cfg predicates and arbitrary user macros remain false.
        """
        words = self.words()
        if words in (["cfg", "(", "test", ")"], ["test"]):
            return True
        if words[:4] != ["tokio", ":", ":", "test"]:
            return False
        return len(words) == 4 or (words[4] == "(" and words[-1] == ")")

    def path_value(self) -> str | None:
        """Return an exact path assignment's plain literal, otherwise None."""
        if len(self.tokens) != 3:
            return None
        if self.words()[:2] != ["path", "="]:
            return None
        return self.tokens[2].path_value()

    def mentions_path(self) -> bool:
        """Return whether path syntax occurs, including inside cfg_attr."""
        return "path" in self.words()

    def unknown_path_at(self, index: int) -> bool:
        """Return whether a path at a valid token index lacks a literal proof."""
        if self.tokens[index].text != "path":
            return False
        assignment = self.tokens[index + 1 : index + 3]
        if len(assignment) != 2:
            return True
        return assignment[0].text != "=" or assignment[1].path_value() is None

    def has_unknown_path(self) -> bool:
        """Return whether any path expression requires conservative ownership."""
        return any(self.unknown_path_at(index) for index in range(len(self.tokens)))

    def possible_paths(self) -> list[str]:
        """Return plain literals that may be path targets in ambiguous syntax."""
        values = (token.path_value() for token in self.tokens)
        return [value for value in values if value is not None]


@dataclass(frozen=True)
class AttributeGroup:
    """Adjacent attributes of the same inner/outer kind.

    Attributes
    ----------
    start, next_index : int
        Token indices of the opening hash and first following item token.
    inner : bool
        Whether the group applies to its enclosing scope rather than an item.
    attributes : list of RustAttribute
        Ordered attributes; multiple cfg restrictions are conjunctive.
    """

    start: int
    next_index: int
    inner: bool = field(kw_only=True)
    attributes: list[RustAttribute]

    def proves_test(self) -> bool:
        """Return whether a conjunctive attribute proves test-only context."""
        return any(attribute.proves_test() for attribute in self.attributes)


@dataclass(frozen=True)
class ModuleDeclaration:
    """A syntactically recognised inline or external module declaration.

    Attributes
    ----------
    name : str
        Declared module identifier.
    index, body_index : int
        Token indices of ``mod`` and its following brace/semicolon position.
    attributes : list of RustAttribute
        Attributes preceding the declaration, including before visibility.
    """

    name: str
    index: int
    body_index: int
    attributes: list[RustAttribute]
