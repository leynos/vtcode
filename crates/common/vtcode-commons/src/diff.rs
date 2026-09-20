//! Diff utilities for generating structured diffs.

use hashbrown::HashMap;
use serde::Serialize;
use std::cmp::min;

/// Represents a chunk of text in a diff (Equal, Delete, or Insert).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunk<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Compute an optimal diff between two strings using Myers algorithm.
#[inline]
pub fn compute_diff_chunks<'a>(old: &'a str, new: &'a str) -> Vec<Chunk<'a>> {
    if old.is_empty() && new.is_empty() {
        return Vec::with_capacity(0);
    }
    if old.is_empty() {
        return vec![Chunk::Insert(new)];
    }
    if new.is_empty() {
        return vec![Chunk::Delete(old)];
    }

    // Strip common prefix first (optimization).
    let prefix_byte_len: usize = old
        .chars()
        .zip(new.chars())
        .take_while(|(o, n)| o == n)
        .map(|(c, _)| c.len_utf8())
        .sum();

    // Strip common suffix on the remaining text.
    let old_rest = old.get(prefix_byte_len..).unwrap_or_default();
    let new_rest = new.get(prefix_byte_len..).unwrap_or_default();

    let suffix_byte_len: usize = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(o, n)| o == n)
        .map(|(c, _)| c.len_utf8())
        .sum();

    let old_middle_end = old_rest.len() - suffix_byte_len;
    let new_middle_end = new_rest.len() - suffix_byte_len;

    let old_middle = old_rest.get(..old_middle_end).unwrap_or_default();
    let new_middle = new_rest.get(..new_middle_end).unwrap_or_default();

    let mut result = Vec::with_capacity(old_middle.len() + new_middle.len());

    // Add common prefix
    if prefix_byte_len > 0 {
        result.push(Chunk::Equal(old.get(..prefix_byte_len).unwrap_or_default()));
    }

    // Compute optimal diff for the middle section.
    append_middle_diff(old_middle, new_middle, &mut result);

    // Add common suffix
    if suffix_byte_len > 0 {
        result.push(Chunk::Equal(old.get(old.len() - suffix_byte_len..).unwrap_or_default()));
    }

    result
}

fn append_middle_diff<'a>(old_middle: &'a str, new_middle: &'a str, result: &mut Vec<Chunk<'a>>) {
    if old_middle.is_empty() && new_middle.is_empty() {
        return;
    }
    let old_chars: Vec<char> = old_middle.chars().collect();
    let new_chars: Vec<char> = new_middle.chars().collect();
    let edits = myers_diff(&old_chars, &new_chars);
    let mut state = MiddleDiff::new(old_middle, new_middle, result);
    for edit in edits {
        if !state.apply(edit) {
            break;
        }
    }
    state.finish();
}

struct MiddleDiff<'a, 'b> {
    old_middle: &'a str,
    new_middle: &'a str,
    old_chars: Vec<char>,
    new_chars: Vec<char>,
    old_byte_starts: Vec<usize>,
    new_byte_starts: Vec<usize>,
    old_pos: usize,
    new_pos: usize,
    equal_run_start: Option<usize>,
    result: &'b mut Vec<Chunk<'a>>,
}

impl<'a, 'b> MiddleDiff<'a, 'b> {
    fn new(old_middle: &'a str, new_middle: &'a str, result: &'b mut Vec<Chunk<'a>>) -> Self {
        Self {
            old_chars: old_middle.chars().collect(),
            new_chars: new_middle.chars().collect(),
            old_byte_starts: old_middle.char_indices().map(|(idx, _)| idx).collect(),
            new_byte_starts: new_middle.char_indices().map(|(idx, _)| idx).collect(),
            old_middle,
            new_middle,
            old_pos: 0,
            new_pos: 0,
            equal_run_start: None,
            result,
        }
    }

    fn apply(&mut self, edit: Edit) -> bool {
        if edit == Edit::Equal {
            self.equal_run_start.get_or_insert(self.old_pos);
            self.old_pos += 1;
            self.new_pos += 1;
            return true;
        }
        if edit == Edit::Delete {
            return self.append_delete();
        }
        self.append_insert()
    }

    fn append_delete(&mut self) -> bool {
        self.flush_equal(self.old_pos);
        let Some(ch) = self.old_chars.get(self.old_pos).copied() else {
            return false;
        };
        let Some(byte_start) = self.old_byte_starts.get(self.old_pos).copied() else {
            return false;
        };
        let byte_end = byte_start + ch.len_utf8();
        self.result.push(Chunk::Delete(self.old_middle.get(byte_start..byte_end).unwrap_or_default()));
        self.old_pos += 1;
        true
    }

    fn append_insert(&mut self) -> bool {
        self.flush_equal(self.old_pos.min(self.old_byte_starts.len()));
        let Some(ch) = self.new_chars.get(self.new_pos).copied() else {
            return false;
        };
        let Some(byte_start) = self.new_byte_starts.get(self.new_pos).copied() else {
            return false;
        };
        let byte_end = byte_start + ch.len_utf8();
        self.result.push(Chunk::Insert(self.new_middle.get(byte_start..byte_end).unwrap_or_default()));
        self.new_pos += 1;
        true
    }

    fn flush_equal(&mut self, end: usize) {
        let Some(start) = self.equal_run_start.take() else {
            return;
        };
        let Some(byte_start) = self.old_byte_starts.get(start).copied() else {
            return;
        };
        let byte_end = self.old_byte_starts.get(end).copied().unwrap_or(self.old_middle.len());
        if byte_start < byte_end {
            self.result.push(Chunk::Equal(self.old_middle.get(byte_start..byte_end).unwrap_or_default()));
        }
    }

    fn finish(&mut self) {
        self.flush_equal(self.old_byte_starts.len());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Equal,
    Delete,
    Insert,
}

/// Advance along matching characters. Extracted from `myers_diff` so the
/// compiler sees a tight leaf loop with no surrounding state, enabling better
/// register allocation and (in some cases) auto-vectorization heuristics.
#[inline]
fn advance_matching(old: &[char], new: &[char], mut x: usize, mut y: usize) -> (usize, usize) {
    while x < old.len() && y < new.len() && old.get(x) == new.get(y) {
        x += 1;
        y += 1;
    }
    (x, y)
}

/// Erase the trailing equal run during backtracking. Same rationale as
/// `advance_matching` — a focused leaf function that the compiler can
/// optimize in isolation.
/// Returns the final `(x, y)` position after removing equal edits.
#[inline]
fn backtrack_equal_run(
    mut x: usize,
    mut y: usize,
    move_x: usize,
    move_y: usize,
    edits: &mut Vec<Edit>,
) -> (usize, usize) {
    while x > move_x && y > move_y {
        edits.push(Edit::Equal);
        x -= 1;
        y -= 1;
    }
    (x, y)
}

fn myers_diff(old: &[char], new: &[char]) -> Vec<Edit> {
    let old_len = old.len();
    let new_len = new.len();

    if old_len == 0 {
        return vec![Edit::Insert; new_len];
    }
    if new_len == 0 {
        return vec![Edit::Delete; old_len];
    }

    let max_distance = old_len
        .saturating_add(new_len)
        .min(usize::try_from(i32::MAX).unwrap_or(usize::MAX));
    let max_distance_i32 = i32::try_from(max_distance).unwrap_or_default();
    let mut furthest_x = vec![0; 2 * max_distance + 1];
    let mut v_index = vec![0usize; (max_distance + 1) * (2 * max_distance + 1)];
    let row_len = 2 * max_distance + 1;

    if let Some(value) = furthest_x.get_mut(max_distance) {
        *value = 0;
    }

    for distance in 0..=max_distance {
        let distance_i32 = i32::try_from(distance).unwrap_or_default();
        let row_start = distance * row_len;
        for diagonal in (-distance_i32..=distance_i32).step_by(2) {
            let diagonal_idx = usize::try_from(diagonal + max_distance_i32).unwrap_or_default();

            let candidate_x = if diagonal == -distance_i32
                || (diagonal != distance_i32
                && furthest_x.get(diagonal_idx - 1).copied().unwrap_or_default()
                    < furthest_x.get(diagonal_idx + 1).copied().unwrap_or_default())
            {
                furthest_x.get(diagonal_idx + 1).copied().unwrap_or_default()
            } else {
                furthest_x.get(diagonal_idx - 1).copied().unwrap_or_default() + 1
            };

            let mut x_position = candidate_x;
            let mut y_position = usize::try_from(
                i64::try_from(candidate_x).unwrap_or(i64::MAX) - i64::from(diagonal),
            )
                .unwrap_or_default();

            (x_position, y_position) = advance_matching(old, new, x_position, y_position);

            if let Some(value) = furthest_x.get_mut(diagonal_idx) {
                *value = x_position;
            }
            if let Some(value) = v_index.get_mut(row_start + diagonal_idx) {
                *value = x_position;
            }

            if x_position >= old_len && y_position >= new_len {
                return backtrack_myers(old, new, &v_index, distance, diagonal, max_distance);
            }
        }
    }

    vec![]
}

fn backtrack_myers(
    old: &[char],
    new: &[char],
    v_index: &[usize],
    distance: usize,
    mut diagonal: i32,
    max_distance: usize,
) -> Vec<Edit> {
    let mut edits = Vec::with_capacity(old.len() + new.len());
    let mut x = old.len();
    let mut y = new.len();
    let max_distance_i32 = i32::try_from(max_distance).unwrap_or_default();
    let row_len = 2 * max_distance + 1;

    for current_distance in (0..=distance).rev() {
        if current_distance == 0 {
            while x > 0 && y > 0 {
                edits.push(Edit::Equal);
                x -= 1;
                y -= 1;
            }
            break;
        }

        let diagonal_idx = usize::try_from(diagonal + max_distance_i32).unwrap_or_default();
        let prev_row_start = (current_distance - 1) * row_len;

        let current_distance_i32 = i32::try_from(current_distance).unwrap_or_default();
        let previous_diagonal = if diagonal == current_distance_i32.wrapping_neg()
            || (diagonal != current_distance_i32
                && v_index
                    .get(prev_row_start + diagonal_idx - 1)
                    .copied()
                    .unwrap_or_default()
                    < v_index
                        .get(prev_row_start + diagonal_idx + 1)
                        .copied()
                        .unwrap_or_default())
        {
            diagonal + 1
        } else {
            diagonal - 1
        };

        let previous_diagonal_idx =
            usize::try_from(previous_diagonal + max_distance_i32).unwrap_or_default();
        let previous_x = v_index
            .get(prev_row_start + previous_diagonal_idx)
            .copied()
            .unwrap_or_default();
        let previous_y = usize::try_from(
            i64::try_from(previous_x).unwrap_or(i64::MAX) - i64::from(previous_diagonal),
        )
            .unwrap_or_default();

        let (move_x, move_y) = if previous_diagonal == diagonal + 1 {
            (previous_x, previous_y + 1)
        } else {
            (previous_x + 1, previous_y)
        };

        (x, y) = backtrack_equal_run(x, y, move_x, move_y, &mut edits);

        if previous_diagonal == diagonal + 1 {
            edits.push(Edit::Insert);
            y -= 1;
        } else {
            edits.push(Edit::Delete);
            x -= 1;
        }

        diagonal = previous_diagonal;
    }

    edits.reverse();
    edits
}

/// Options for diff generation.
#[derive(Debug, Clone)]
pub struct DiffOptions<'a> {
    pub context_lines: usize,
    pub old_label: Option<&'a str>,
    pub new_label: Option<&'a str>,
    pub missing_newline_hint: bool,
}

impl Default for DiffOptions<'_> {
    fn default() -> Self {
        Self {
            context_lines: 3,
            old_label: None,
            new_label: None,
            missing_newline_hint: true,
        }
    }
}

/// A diff rendered with both structured hunks and formatted text.
#[derive(Debug, Clone, Serialize)]
pub struct DiffBundle {
    pub hunks: Vec<DiffHunk>,
    pub formatted: String,
    pub is_empty: bool,
}

/// A diff hunk with metadata for old/new ranges.
#[derive(Debug, Clone, Serialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<DiffLine>,
}

/// A single diff line annotated with metadata and type.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

/// Metadata for a single line inside a diff hunk.
#[derive(Debug, Clone, Serialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

/// Compute a structured diff bundle.
pub fn compute_diff<F>(old: &str, new: &str, options: DiffOptions<'_>, formatter: F) -> DiffBundle
where
    F: FnOnce(&[DiffHunk], &DiffOptions<'_>) -> String,
{
    let old_lines_owned = split_lines_with_terminator(old);
    let new_lines_owned = split_lines_with_terminator(new);

    let old_refs: Vec<&str> = old_lines_owned.iter().map(|s| s.as_str()).collect();
    let new_refs: Vec<&str> = new_lines_owned.iter().map(|s| s.as_str()).collect();

    let records = collect_line_records(&old_refs, &new_refs);
    let has_changes = records
        .iter()
        .any(|record| matches!(record.kind, DiffLineKind::Addition | DiffLineKind::Deletion));

    let hunks = if has_changes {
        build_hunks(&records, options.context_lines)
    } else {
        Vec::new()
    };

    let rendered = if hunks.is_empty() {
        String::new()
    } else {
        formatter(&hunks, &options)
    };

    DiffBundle { hunks, formatted: rendered, is_empty: !has_changes }
}

fn split_lines_with_terminator(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::with_capacity(0);
    }

    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut line_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let Some(&byte) = bytes.get(index) else {
            break;
        };
        if byte != b'\n' && byte != b'\r' {
            index += 1;
            continue;
        }
        let line_end = if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index + 2
        } else {
            index + 1
        };
        lines.push(text.get(line_start..line_end).unwrap_or_default().to_string());
        line_start = line_end;
        index = line_end;
    }
    if line_start < text.len() {
        lines.push(text.get(line_start..).unwrap_or_default().to_string());
    }

    lines
}

#[inline]
fn collect_line_records<'a>(old_lines: &'a [&'a str], new_lines: &'a [&'a str]) -> Vec<LineRecord<'a>> {
    let (old_encoded, new_encoded) = encode_line_sequences(old_lines, new_lines);
    let mut records = Vec::with_capacity(old_lines.len() + new_lines.len());
    let mut old_index = 0u32;
    let mut new_index = 0u32;

    for chunk in compute_diff_chunks(old_encoded.as_str(), new_encoded.as_str()) {
        match chunk {
            Chunk::Equal(text) => {
                for _ in text.chars() {
                    let old_line = old_index + 1;
                    let new_line = new_index + 1;
                    let line = old_lines
                        .get(usize::try_from(old_index).unwrap_or_default())
                        .copied()
                        .unwrap_or_default();
                    records.push(LineRecord {
                        kind: DiffLineKind::Context,
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                        text: line,
                        anchor_old: old_line,
                        anchor_new: new_line,
                    });
                    old_index += 1;
                    new_index += 1;
                }
            }
            Chunk::Delete(text) => {
                for _ in text.chars() {
                    let old_line = old_index + 1;
                    let anchor_new = new_index + 1;
                    let line = old_lines
                        .get(usize::try_from(old_index).unwrap_or_default())
                        .copied()
                        .unwrap_or_default();
                    records.push(LineRecord {
                        kind: DiffLineKind::Deletion,
                        old_line: Some(old_line),
                        new_line: None,
                        text: line,
                        anchor_old: old_line,
                        anchor_new,
                    });
                    old_index += 1;
                }
            }
            Chunk::Insert(text) => {
                for _ in text.chars() {
                    let new_line = new_index + 1;
                    let anchor_old = old_index + 1;
                    let line = new_lines
                        .get(usize::try_from(new_index).unwrap_or_default())
                        .copied()
                        .unwrap_or_default();
                    records.push(LineRecord {
                        kind: DiffLineKind::Addition,
                        old_line: None,
                        new_line: Some(new_line),
                        text: line,
                        anchor_old,
                        anchor_new: new_line,
                    });
                    new_index += 1;
                }
            }
        }
    }

    records
}

fn encode_line_sequences<'a>(old_lines: &'a [&'a str], new_lines: &'a [&'a str]) -> (String, String) {
    let mut token_map: HashMap<&'a str, char> = HashMap::new();
    let mut next_codepoint: u32 = 0;

    let old_encoded = encode_line_list(old_lines, &mut token_map, &mut next_codepoint);
    let new_encoded = encode_line_list(new_lines, &mut token_map, &mut next_codepoint);

    (old_encoded, new_encoded)
}

fn encode_line_list<'a>(lines: &'a [&'a str], map: &mut HashMap<&'a str, char>, next_codepoint: &mut u32) -> String {
    let mut encoded = String::with_capacity(lines.len());
    for &line in lines {
        let token = if let Some(&value) = map.get(line) {
            value
        } else {
            let Some(ch) = next_token_char(next_codepoint) else {
                break;
            };
            let _previous = map.insert(line, ch);
            ch
        };
        encoded.push(token);
    }
    encoded
}

fn next_token_char(counter: &mut u32) -> Option<char> {
    while *counter <= 0x0010_FFFF {
        let candidate = *counter;
        *counter += 1;
        if (0xD800..=0xDFFF).contains(&candidate) {
            continue;
        }
        if let Some(ch) = char::from_u32(candidate) {
            return Some(ch);
        }
    }
    None
}

#[derive(Debug)]
struct LineRecord<'a> {
    kind: DiffLineKind,
    old_line: Option<u32>,
    new_line: Option<u32>,
    text: &'a str,
    anchor_old: u32,
    anchor_new: u32,
}

fn build_hunks(records: &[LineRecord<'_>], context: usize) -> Vec<DiffHunk> {
    if records.is_empty() {
        return Vec::new();
    }

    let ranges = compute_hunk_ranges(records, context);
    let mut hunks = Vec::with_capacity(ranges.len());

    for (start, end) in ranges {
        let slice = records.get(start..=end).unwrap_or_default();

        let Some(first) = slice.first() else {
            continue;
        };
        let old_start = usize::try_from(first.old_line.unwrap_or(first.anchor_old).max(1)).unwrap_or(usize::MAX);
        let new_start = usize::try_from(first.new_line.unwrap_or(first.anchor_new).max(1)).unwrap_or(usize::MAX);

        let old_lines = slice
            .iter()
            .filter(|r| matches!(r.kind, DiffLineKind::Context | DiffLineKind::Deletion))
            .count();
        let new_lines = slice
            .iter()
            .filter(|r| matches!(r.kind, DiffLineKind::Context | DiffLineKind::Addition))
            .count();

        let lines = slice
            .iter()
            .map(|record| DiffLine {
                kind: record.kind,
                old_line: record.old_line,
                new_line: record.new_line,
                text: record.text.to_string(),
            })
            .collect();

        hunks.push(DiffHunk { old_start, old_lines, new_start, new_lines, lines });
    }

    hunks
}

fn compute_hunk_ranges(records: &[LineRecord<'_>], context: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(4);
    let mut current_start: Option<usize> = None;
    let mut current_end: usize = 0;

    for (idx, record) in records.iter().enumerate() {
        if record.kind != DiffLineKind::Context {
            let start = idx.saturating_sub(context);
            let end = min(idx + context, records.len().saturating_sub(1));

            match current_start.take() {
                // Close the previous range if this change is beyond its context window.
                Some(existing_start) if idx > current_end => {
                    ranges.push((existing_start, current_end));
                    current_start = Some(start);
                    current_end = end;
                }
                Some(existing_start) => {
                    current_start = Some(start.min(existing_start));
                    current_end = end.max(current_end);
                }
                None => {
                    current_start = Some(start);
                    current_end = end;
                }
            }
        } else if let Some(start) = current_start
            && idx > current_end
        {
            ranges.push((start, current_end));
            current_start = None;
        }
    }

    if let Some(start) = current_start {
        ranges.push((start, current_end));
    }

    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── compute_diff_chunks ──────────────────────────────────────────

    #[test]
    fn chunks_both_empty() {
        let chunks = compute_diff_chunks("", "");
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunks_old_empty() {
        let chunks = compute_diff_chunks("", "hello");
        assert_eq!(chunks, vec![Chunk::Insert("hello")]);
    }

    #[test]
    fn chunks_new_empty() {
        let chunks = compute_diff_chunks("hello", "");
        assert_eq!(chunks, vec![Chunk::Delete("hello")]);
    }

    #[test]
    fn chunks_identical() {
        let chunks = compute_diff_chunks("abc", "abc");
        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0], Chunk::Equal("abc")));
    }

    #[test]
    fn chunks_single_insertion() {
        let chunks = compute_diff_chunks("ac", "abc");
        // Common prefix "a", insert "b", common suffix "c"
        assert_eq!(chunks.len(), 3);
        assert!(matches!(chunks[0], Chunk::Equal("a")));
        assert!(matches!(chunks[1], Chunk::Insert("b")));
        assert!(matches!(chunks[2], Chunk::Equal("c")));
    }

    #[test]
    fn chunks_single_deletion() {
        let chunks = compute_diff_chunks("abc", "ac");
        assert_eq!(chunks.len(), 3);
        assert!(matches!(chunks[0], Chunk::Equal("a")));
        assert!(matches!(chunks[1], Chunk::Delete("b")));
        assert!(matches!(chunks[2], Chunk::Equal("c")));
    }

    #[test]
    fn chunks_replacement() {
        let chunks = compute_diff_chunks("abc", "axc");
        // Equal("a"), Delete("b"), Insert("x"), Equal("c")
        assert_eq!(chunks.len(), 4);
        assert!(matches!(chunks[0], Chunk::Equal("a")));
        assert!(matches!(chunks[1], Chunk::Delete("b")));
        assert!(matches!(chunks[2], Chunk::Insert("x")));
        assert!(matches!(chunks[3], Chunk::Equal("c")));
    }

    #[test]
    fn chunks_completely_different() {
        let chunks = compute_diff_chunks("aaa", "bbb");
        // No common prefix or suffix
        assert!(!chunks.is_empty());
        // All old chars deleted, all new chars inserted
        let deletes: usize = chunks.iter().filter(|c| matches!(c, Chunk::Delete(_))).count();
        let inserts: usize = chunks.iter().filter(|c| matches!(c, Chunk::Insert(_))).count();
        assert!(deletes > 0 || inserts > 0);
    }

    #[test]
    fn chunks_multiline() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline modified\nline3\n";
        let chunks = compute_diff_chunks(old, new);

        // Should have at least some Equal chunks for the unchanged lines
        let has_equal = chunks.iter().any(|c| matches!(c, Chunk::Equal(_)));
        assert!(has_equal);

        // Should have a delete and insert for the changed line
        let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
        let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
        assert!(has_delete || has_insert);
    }

    #[test]
    fn chunks_unicode() {
        let old = "hello \u{00e9}l\u{00e8}ve";
        let new = "hello \u{00e9}l\u{00e8}ve you";
        let chunks = compute_diff_chunks(old, new);

        // Common prefix should include unicode chars
        let prefix = match &chunks[0] {
            Chunk::Equal(s) => s,
            _ => panic!("expected Equal prefix"),
        };
        assert!(prefix.starts_with("hello "));
    }

    #[test]
    fn chunks_append_only() {
        let old = "a\nb\n";
        let new = "a\nb\nc\nd\n";
        let chunks = compute_diff_chunks(old, new);
        let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
        assert!(has_insert);
    }

    #[test]
    fn chunks_remove_only() {
        let old = "a\nb\nc\n";
        let new = "a\n";
        let chunks = compute_diff_chunks(old, new);
        let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
        assert!(has_delete);
    }

    // ── compute_diff ─────────────────────────────────────────────────

    fn identity_formatter(hunks: &[DiffHunk], _opts: &DiffOptions<'_>) -> String {
        hunks
            .iter()
            .flat_map(|h| h.lines.iter().map(|l| l.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn diff_identical_content() {
        let result = compute_diff("hello\n", "hello\n", DiffOptions::default(), identity_formatter);
        assert!(result.is_empty);
        assert!(result.hunks.is_empty());
        assert!(result.formatted.is_empty());
    }

    #[test]
    fn diff_empty_both() {
        let result = compute_diff("", "", DiffOptions::default(), identity_formatter);
        assert!(result.is_empty);
        assert!(result.hunks.is_empty());
    }

    #[test]
    fn diff_old_empty() {
        let result = compute_diff("", "line1\nline2\n", DiffOptions::default(), identity_formatter);
        assert!(!result.is_empty);
        assert!(!result.hunks.is_empty());
        // All lines should be additions
        for hunk in &result.hunks {
            for line in &hunk.lines {
                assert_eq!(line.kind, DiffLineKind::Addition);
            }
        }
    }

    #[test]
    fn diff_new_empty() {
        let result = compute_diff("line1\nline2\n", "", DiffOptions::default(), identity_formatter);
        assert!(!result.is_empty);
        assert!(!result.hunks.is_empty());
        for hunk in &result.hunks {
            for line in &hunk.lines {
                assert_eq!(line.kind, DiffLineKind::Deletion);
            }
        }
    }

    #[test]
    fn diff_single_line_change() {
        let old = "aaa\nbbb\nccc\n";
        let new = "aaa\nxxx\nccc\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

        assert!(!result.is_empty);
        assert_eq!(result.hunks.len(), 1);

        let hunk = &result.hunks[0];
        // Should have context lines for aaa and ccc, plus the change
        let kinds: Vec<DiffLineKind> = hunk.lines.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&DiffLineKind::Context));
        assert!(kinds.contains(&DiffLineKind::Deletion));
        assert!(kinds.contains(&DiffLineKind::Addition));
    }

    #[test]
    fn diff_line_numbers() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline2 modified\nline3\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

        let hunk = &result.hunks[0];
        // Context lines should have both old_line and new_line
        for line in &hunk.lines {
            if line.kind == DiffLineKind::Context {
                assert!(line.old_line.is_some());
                assert!(line.new_line.is_some());
            }
        }
        // Deletion should have old_line but no new_line
        for line in &hunk.lines {
            if line.kind == DiffLineKind::Deletion {
                assert!(line.old_line.is_some());
                assert!(line.new_line.is_none());
            }
        }
        // Addition should have new_line but no old_line
        for line in &hunk.lines {
            if line.kind == DiffLineKind::Addition {
                assert!(line.old_line.is_none());
                assert!(line.new_line.is_some());
            }
        }
    }

    #[test]
    fn diff_context_lines_zero() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nX\nd\ne\n";
        let opts = DiffOptions { context_lines: 0, ..DiffOptions::default() };
        let result = compute_diff(old, new, opts, identity_formatter);

        assert!(!result.is_empty);
        // With 0 context, only the changed line and its neighbours should appear
        let hunk = &result.hunks[0];
        // Should be minimal: just the deletion and addition
        let context_count = hunk.lines.iter().filter(|l| l.kind == DiffLineKind::Context).count();
        assert!(context_count <= 2); // At most one context line on each side
    }

    #[test]
    fn diff_context_lines_large() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nX\nd\ne\n";
        let opts = DiffOptions { context_lines: 10, ..DiffOptions::default() };
        let result = compute_diff(old, new, opts, identity_formatter);

        assert!(!result.is_empty);
        // With 10 context lines and only 6 total lines (trailing newline creates 6th), all lines appear
        let hunk = &result.hunks[0];
        assert_eq!(hunk.lines.len(), 6);
    }

    #[test]
    fn diff_hunk_metadata() {
        let old = "aaa\nbbb\nccc\n";
        let new = "aaa\nxxx\nccc\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

        let hunk = &result.hunks[0];
        assert!(hunk.old_start >= 1);
        assert!(hunk.new_start >= 1);
        assert!(hunk.old_lines > 0);
        assert!(hunk.new_lines > 0);
        assert!(!hunk.lines.is_empty());
    }

    #[test]
    fn diff_hunk_start_uses_the_first_represented_line() {
        let result = compute_diff("existing\n", "inserted\nexisting\n", DiffOptions::default(), identity_formatter);
        let hunk = &result.hunks[0];

        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.old_lines, 1);
        assert_eq!(hunk.new_lines, 2);
    }

    #[test]
    fn diff_multiple_hunks() {
        // Insert in first half and insert in second half with small context => two hunks
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nINSERTED1\nb\nc\nd\ne\nf\ng\nINSERTED2\nh\n";
        let opts = DiffOptions { context_lines: 1, ..DiffOptions::default() };
        let result = compute_diff(old, new, opts, identity_formatter);

        assert!(!result.is_empty);
        assert!(result.hunks.len() >= 2, "expected at least 2 hunks, got {}", result.hunks.len());
    }

    #[test]
    fn diff_formatter_called() {
        let old = "aaa\n";
        let new = "bbb\n";
        let mut called = false;
        let formatter = |hunks: &[DiffHunk], _opts: &DiffOptions<'_>| -> String {
            called = true;
            hunks
                .iter()
                .flat_map(|h| h.lines.iter().map(|l| l.text.clone()))
                .collect::<Vec<_>>()
                .join("")
        };

        let result = compute_diff(old, new, DiffOptions::default(), formatter);
        assert!(called);
        assert!(!result.formatted.is_empty());
    }

    #[test]
    fn diff_formatter_not_called_when_empty() {
        let mut called = false;
        let formatter = |_hunks: &[DiffHunk], _opts: &DiffOptions<'_>| -> String {
            called = true;
            String::new()
        };

        let result = compute_diff("same\n", "same\n", DiffOptions::default(), formatter);
        assert!(!called);
        assert!(result.formatted.is_empty());
    }

    #[test]
    fn diff_options_labels() {
        let old = "aaa\n";
        let new = "bbb\n";
        let opts = DiffOptions {
            old_label: Some("old.txt"),
            new_label: Some("new.txt"),
            ..DiffOptions::default()
        };
        let result = compute_diff(old, new, opts, identity_formatter);
        assert!(!result.is_empty);
        // Labels are passed to formatter but don't affect hunks
        assert_eq!(result.hunks.len(), 1);
    }

    #[test]
    fn diff_insertion_only() {
        let old = "line1\nline3\n";
        let new = "line1\nline2\nline3\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

        assert!(!result.is_empty);
        let additions: Vec<&DiffLine> = result
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == DiffLineKind::Addition)
            .collect();
        assert_eq!(additions.len(), 1);
        assert_eq!(additions[0].text, "line2\n");
    }

    #[test]
    fn diff_preserves_crlf_and_cr_line_endings() {
        let crlf = compute_diff("one\r\ntwo\r\n", "one\r\nchanged\r\n", DiffOptions::default(), identity_formatter);
        assert_eq!(
            crlf.hunks[0]
                .lines
                .iter()
                .map(|line| (line.kind, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (DiffLineKind::Context, "one\r\n"),
                (DiffLineKind::Deletion, "two\r\n"),
                (DiffLineKind::Addition, "changed\r\n"),
            ]
        );

        let cr = compute_diff("one\rtwo\r", "one\rchanged\r", DiffOptions::default(), identity_formatter);
        assert_eq!(
            cr.hunks[0]
                .lines
                .iter()
                .map(|line| (line.kind, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (DiffLineKind::Context, "one\r"),
                (DiffLineKind::Deletion, "two\r"),
                (DiffLineKind::Addition, "changed\r"),
            ]
        );
    }

    #[test]
    fn diff_deletion_only() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline3\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

        assert!(!result.is_empty);
        let deletions: Vec<&DiffLine> = result
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == DiffLineKind::Deletion)
            .collect();
        assert_eq!(deletions.len(), 1);
        assert_eq!(deletions[0].text, "line2\n");
    }

    // ── DiffBundle serialization ─────────────────────────────────────

    #[test]
    fn diff_bundle_serializes() {
        let old = "aaa\nbbb\n";
        let new = "aaa\nxxx\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

        let json = serde_json::to_value(&result).expect("diff bundle fixture must serialize");
        assert_eq!(json.get("hunks").and_then(serde_json::Value::as_array).map(Vec::len), Some(1));
        assert_eq!(json.get("formatted").and_then(serde_json::Value::as_str), Some(result.formatted.as_str()));
        assert_eq!(json.get("is_empty").and_then(serde_json::Value::as_bool), Some(false));
    }

    #[test]
    fn diff_hunk_serializes() {
        let hunk = DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            lines: vec![DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(1),
                new_line: Some(1),
                text: "hello\n".to_string(),
            }],
        };
        let json = serde_json::to_value(&hunk).expect("diff hunk fixture must serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "old_start": 1,
                "old_lines": 2,
                "new_start": 1,
                "new_lines": 2,
                "lines": [{"kind": "context", "old_line": 1, "new_line": 1, "text": "hello\n"}],
            })
        );
    }

    #[test]
    fn diff_line_kind_serializes() {
        for (kind, wire_name) in [
            (DiffLineKind::Context, "context"),
            (DiffLineKind::Addition, "addition"),
            (DiffLineKind::Deletion, "deletion"),
        ] {
            let encoded = serde_json::to_value(kind).expect("diff line kind fixture must serialize");
            assert_eq!(encoded.as_str(), Some(wire_name));
        }
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn chunks_very_long_identical() {
        let text = "x".repeat(10_000);
        let chunks = compute_diff_chunks(&text, &text);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0], Chunk::Equal(_)));
    }

    #[test]
    fn chunks_single_char_diff() {
        let chunks = compute_diff_chunks("a", "b");
        assert!(!chunks.is_empty());
        let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
        let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
        assert!(has_delete && has_insert);
    }

    #[test]
    fn diff_no_trailing_newline() {
        let old = "line1\nline2";
        let new = "line1\nline2\n";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);
        assert!(!result.is_empty);
    }

    #[test]
    fn diff_only_newlines_differ() {
        let old = "a\nb\n";
        let new = "a\nb";
        let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);
        assert!(!result.is_empty);
    }

    #[test]
    fn chunks_prefix_suffix_optimization() {
        // Verify that common prefix and suffix are preserved as Equal chunks.
        // Myers works character-by-character, so the middle diff is char-level.
        let old = "AAAA BBBB CCCC";
        let new = "AAAA DDDD CCCC";
        let chunks = compute_diff_chunks(old, new);

        // First chunk should be Equal prefix "AAAA "
        assert!(matches!(&chunks[0], Chunk::Equal(s) if *s == "AAAA "));
        // Last chunk should be Equal suffix " CCCC"
        assert!(matches!(chunks.last().unwrap(), Chunk::Equal(s) if *s == " CCCC"));
        // Middle should contain deletes and inserts (character-level)
        let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
        let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
        assert!(has_delete, "expected Delete chunks in middle");
        assert!(has_insert, "expected Insert chunks in middle");
    }
}
