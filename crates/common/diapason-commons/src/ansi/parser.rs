#![expect(
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "The ANSI parser validates byte positions while walking escape-sequence boundaries."
)]

//! ANSI escape-sequence parsing and stripping implementation.

use crate::ansi_codes::{BEL_BYTE, ESC_BYTE};
use memchr::memchr;

const ESC: u8 = ESC_BYTE;
const BEL: u8 = BEL_BYTE;
const DEL: u8 = 0x7f;
const C1_ST: u8 = 0x9c;
const C1_DCS: u8 = 0x90;
const C1_SOS: u8 = 0x98;
const C1_CSI: u8 = 0x9b;
const C1_OSC: u8 = 0x9d;
const C1_PM: u8 = 0x9e;
const C1_APC: u8 = 0x9f;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const MAX_STRING_SEQUENCE_BYTES: usize = 4096;
const MAX_CSI_SEQUENCE_BYTES: usize = 64;

#[derive(Clone, Copy)]
enum StringSequenceTerminator {
    StOnly,
    BelOrSt,
}

impl StringSequenceTerminator {
    #[inline]
    const fn allows_bel(self) -> bool {
        matches!(self, Self::BelOrSt)
    }
}

#[inline]
fn parse_c1_at(bytes: &[u8], start: usize) -> Option<(u8, usize)> {
    let first = *bytes.get(start)?;
    if (0x80..=0x9f).contains(&first) {
        return Some((first, 1));
    }
    None
}

#[inline]
fn parse_csi(bytes: &[u8], start: usize) -> Option<usize> {
    // ECMA-48 / ISO 6429 CSI grammar:
    // - parameter bytes: 0x30..0x3F
    // - intermediate bytes: 0x20..0x2F
    // - final byte: 0x40..0x7E
    // (See ANSI escape code article on Wikipedia, CSI section.)
    let mut index = start;
    let mut phase = 0u8; // 0=parameter, 1=intermediate
    let mut consumed = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == ESC {
            // VT100: ESC aborts current control sequence and starts a new one.
            return Some(index);
        }
        if byte == CAN || byte == SUB {
            // VT100: CAN/SUB abort current control sequence.
            return Some(index + 1);
        }

        consumed += 1;
        if consumed > MAX_CSI_SEQUENCE_BYTES {
            // Bound malformed or hostile input.
            return Some(index + 1);
        }

        if phase == 0 && (0x30..=0x3f).contains(&byte) {
            index += 1;
            continue;
        }
        if (0x20..=0x2f).contains(&byte) {
            phase = 1;
            index += 1;
            continue;
        }
        if (0x40..=0x7e).contains(&byte) {
            return Some(index + 1);
        }

        // Invalid CSI byte: abort sequence without consuming this byte.
        return Some(index);
    }

    None
}

#[inline]
fn parse_string_sequence(bytes: &[u8], start: usize, terminator: StringSequenceTerminator) -> Option<usize> {
    let mut consumed = 0usize;
    for index in start..bytes.len() {
        if bytes[index] == ESC && !(index + 1 < bytes.len() && bytes[index + 1] == b'\\') {
            // VT100: ESC aborts current sequence and begins a new one.
            return Some(index);
        }
        if bytes[index] == CAN || bytes[index] == SUB {
            return Some(index + 1);
        }

        if let Some((c1, len)) = parse_c1_at(bytes, index)
            && c1 == C1_ST
        {
            return Some(index + len);
        }

        match bytes[index] {
            BEL if terminator.allows_bel() => return Some(index + 1),
            ESC if index + 1 < bytes.len() && bytes[index + 1] == b'\\' => return Some(index + 2),
            _ => {}
        }

        consumed += 1;
        if consumed > MAX_STRING_SEQUENCE_BYTES {
            // Cap unbounded strings when terminator is missing.
            return Some(index + 1);
        }
    }
    None
}

#[inline]
fn push_visible_byte(output: &mut Vec<u8>, byte: u8) {
    if matches!(byte, b'\n' | b'\r' | b'\t') || !(byte < 32 || byte == DEL) {
        output.push(byte);
    }
}

#[inline]
fn parse_ansi_sequence_bytes(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }

    if let Some((c1, c1_len)) = parse_c1_at(bytes, 0) {
        return match c1 {
            C1_CSI => parse_csi(bytes, c1_len),
            C1_OSC => parse_string_sequence(bytes, c1_len, StringSequenceTerminator::BelOrSt),
            C1_DCS | C1_SOS | C1_PM | C1_APC => parse_string_sequence(bytes, c1_len, StringSequenceTerminator::StOnly),
            _ => Some(c1_len),
        };
    }

    match bytes[0] {
        ESC => {
            if bytes.len() < 2 {
                return None;
            }

            match bytes[1] {
                b'[' => parse_csi(bytes, 2),
                b']' => parse_string_sequence(bytes, 2, StringSequenceTerminator::BelOrSt),
                b'P' | b'^' | b'_' | b'X' => parse_string_sequence(bytes, 2, StringSequenceTerminator::StOnly),
                // Three-byte sequences: ESC + intermediate + final
                // ESC SP {F,G,L,M,N} — 7/8-bit controls, ANSI conformance
                // ESC # {3,4,5,6,8} — DEC line attributes / screen alignment
                // ESC % {@ ,G} — character set selection (ISO 2022)
                // ESC ( C / ESC ) C / ESC * C / ESC + C — G0-G3 designation
                b' ' | b'#' | b'%' | b'(' | b')' | b'*' | b'+' => {
                    if bytes.len() > 2 {
                        Some(3)
                    } else {
                        None
                    }
                }
                next if next < 128 => Some(2),
                _ => Some(1),
            }
        }
        _ => None,
    }
}

/// Strip ANSI escape codes from text, returning a borrowed `Cow` when the
/// input contains no ESC byte.  This is the preferred API for call-sites that
/// want to avoid allocation on the common "no ANSI codes" path.
pub fn strip_ansi_codes(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\x1b') {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(strip_ansi(text))
}

/// Strip ANSI escape codes from text, keeping only plain text
pub fn strip_ansi(text: &str) -> String {
    let mut output = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let next_esc = memchr(ESC, &bytes[i..]).map_or(bytes.len(), |offset| i + offset);
        // Pre-slice to avoid bounds checks in the inner loop — the range
        // i..next_esc is provably within bytes[..].
        for &b in &bytes[i..next_esc] {
            push_visible_byte(&mut output, b);
        }
        i = next_esc;

        if i >= bytes.len() {
            break;
        }

        if let Some(len) = parse_ansi_sequence_bytes(&bytes[i..]) {
            i += len;
            continue;
        } else {
            // Incomplete/unterminated control sequence at end of available text.
            break;
        }
    }

    // The output is always valid UTF-8: `push_visible_byte` only filters ASCII
    // control bytes (< 32 except \n/\r/\t, and DEL=127), which cannot be part of
    // a multi-byte UTF-8 sequence (those bytes are all >= 0x80). `from_utf8`
    // reuses the Vec allocation directly; `from_utf8_lossy().into_owned()` would
    // copy even on the valid-UTF-8 fast path.
    String::from_utf8(output).unwrap_or_else(|e| {
        // Defensive fallback — should not happen given the invariant above.
        String::from_utf8_lossy(&e.into_bytes()).into_owned()
    })
}

/// Strip ANSI escape codes from arbitrary bytes, preserving non-control bytes.
///
/// This is the preferred API when input may contain raw C1 (8-bit) controls.
fn strip_ansi_bytes(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let bytes = input;
    let mut i = 0;

    while i < bytes.len() {
        // Pre-slice to the remaining portion so all indexing below shares one bounds edge.
        let rest = &bytes[i..];

        if (rest[0] == ESC || parse_c1_at(bytes, i).is_some())
            && let Some(len) = parse_ansi_sequence_bytes(rest)
        {
            i += len;
            continue;
        }
        if rest[0] == ESC || parse_c1_at(bytes, i).is_some() {
            // Incomplete/unterminated control sequence at end of available text.
            break;
        }

        push_visible_byte(&mut output, rest[0]);
        i += 1;
    }
    output
}

/// Parse and determine the length of the ANSI escape sequence at the start of text
pub fn parse_ansi_sequence(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    parse_ansi_sequence_bytes(bytes)
}

/// Fast ASCII-only ANSI stripping for performance-critical paths
pub fn strip_ansi_ascii_only(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut search_start = 0;
    let mut copy_start = 0;

    while let Some(offset) = memchr(ESC, &bytes[search_start..]) {
        let esc_index = search_start + offset;
        if let Some(len) = parse_ansi_sequence_bytes(&bytes[esc_index..]) {
            if copy_start < esc_index {
                output.push_str(&text[copy_start..esc_index]);
            }
            copy_start = esc_index + len;
            search_start = copy_start;
        } else {
            search_start = esc_index + 1;
        }
    }

    if copy_start < text.len() {
        output.push_str(&text[copy_start..]);
    }

    output
}

/// Detect if text contains unicode characters that need special handling
#[must_use]
pub fn contains_unicode(text: &str) -> bool {
    text.bytes().any(|b| b >= 0x80)
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
