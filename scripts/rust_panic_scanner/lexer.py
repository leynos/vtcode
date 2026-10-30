"""Offset-preserving Rust tokens; strings and comments remain inert."""

from __future__ import annotations

from dataclasses import dataclass, field
import re


RAW_STRING_RE = re.compile(r'(?:br|cr|r)(#*)"')
QUOTED_RE = re.compile(
    r"(?:b|c)?\"|(?:b)?'(?:[^\\'\n]|\\(?:[^\n]|u\{[0-9a-fA-F_]+\}))'"
)
IDENTIFIER_RE = re.compile(r"[a-zA-Z_][a-zA-Z_0-9]*")


@dataclass(frozen=True)
class RustToken:
    """A Rust token with half-open character offsets into its original source.

    Attributes
    ----------
    text : str
        Exact token spelling, including delimiters for literals.
    start, end : int
        Character offsets, not byte offsets or token indices.
    literal : bool
        Whether the token is atomic literal content and cannot supply syntax.
    """

    text: str
    start: int
    end: int
    literal: bool = field(default=False, kw_only=True)

    def path_value(self) -> str | None:
        """Read a plain, unescaped string literal as a possible module path.

        Returns
        -------
        str or None
            Literal contents, or ``None`` when decoding would require a guess.
        """
        if not self.literal or not self.text.startswith('"'):
            return None
        return None if "\\" in self.text else self.text[1:-1]


class RustLexer:
    """Tokenize Rust while keeping comments and literal contents inert.

    Parameters
    ----------
    source : str
        Complete source text. Offsets in returned tokens address this text.

    Notes
    -----
    This lexer recognises nested comments and raw/cooked/character literals;
    it does not expand macros or validate Rust grammar. Unterminated literals
    consume the remaining text instead of interpreting their contents as code.
    """

    def __init__(self, source: str) -> None:
        self.source = source
        self.offset = 0

    def tokens(self) -> list[RustToken]:
        """Consume the remaining input into ordered, non-comment tokens.

        Returns
        -------
        list of RustToken
            Tokens in source order. Calling again after exhaustion returns [].
        """
        tokens: list[RustToken] = []
        while self.offset < len(self.source):
            if not self._skip_ignored():
                tokens.append(self._read_token())
        return tokens

    def _skip_ignored(self) -> bool:
        if self.source[self.offset].isspace():
            self.offset += 1
            return True
        if self.source.startswith("//", self.offset):
            end = self.source.find("\n", self.offset)
            self.offset = len(self.source) if end < 0 else end
            return True
        if self.source.startswith("/*", self.offset):
            self._skip_block_comment()
            return True
        return False

    def _skip_block_comment(self) -> None:
        depth = 1
        self.offset += 2
        changes = {"/*": 1, "*/": -1}
        while depth and self.offset < len(self.source):
            pair = self.source[self.offset : self.offset + 2]
            depth += changes.get(pair, 0)
            self.offset += 2 if pair in changes else 1

    def _take(self, end: int, *, literal: bool = False) -> RustToken:
        token = RustToken(self.source[self.offset:end], self.offset, end, literal=literal)
        self.offset = end
        return token

    def _read_token(self) -> RustToken:
        raw = RAW_STRING_RE.match(self.source, self.offset)
        if raw:
            return self._read_raw(raw)
        quoted = QUOTED_RE.match(self.source, self.offset)
        if quoted:
            return self._read_quoted(quoted)
        word = IDENTIFIER_RE.match(self.source, self.offset)
        return self._take(word.end() if word else self.offset + 1)

    def _read_raw(self, match: re.Match[str]) -> RustToken:
        marker = '"' + match.group(1)
        close = self.source.find(marker, match.end())
        end = len(self.source) if close < 0 else close + len(marker)
        return self._take(end, literal=True)

    def _read_quoted(self, match: re.Match[str]) -> RustToken:
        end = match.end()
        if self.source[end - 1] != "'":
            end = self._string_end(end)
        return self._take(end, literal=True)

    def _string_end(self, offset: int) -> int:
        while offset < len(self.source):
            char = self.source[offset]
            if char == '"':
                return offset + 1
            offset += 2 if char == "\\" else 1
        return offset
