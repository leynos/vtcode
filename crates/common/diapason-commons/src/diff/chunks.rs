//! Character-level Myers diff generation.

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
        self.result
            .push(Chunk::Delete(self.old_middle.get(byte_start..byte_end).unwrap_or_default()));
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
        self.result
            .push(Chunk::Insert(self.new_middle.get(byte_start..byte_end).unwrap_or_default()));
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
            self.result
                .push(Chunk::Equal(self.old_middle.get(byte_start..byte_end).unwrap_or_default()));
        }
    }

    fn finish(&mut self) {
        self.flush_equal(self.old_byte_starts.len());
    }
}

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
            let mut y_position = usize::try_from(i64::try_from(candidate_x).unwrap_or(i64::MAX) - i64::from(diagonal))
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
                && v_index.get(prev_row_start + diagonal_idx - 1).copied().unwrap_or_default()
                    < v_index.get(prev_row_start + diagonal_idx + 1).copied().unwrap_or_default())
        {
            diagonal + 1
        } else {
            diagonal - 1
        };

        let previous_diagonal_idx = usize::try_from(previous_diagonal + max_distance_i32).unwrap_or_default();
        let previous_x = v_index.get(prev_row_start + previous_diagonal_idx).copied().unwrap_or_default();
        let previous_y = usize::try_from(i64::try_from(previous_x).unwrap_or(i64::MAX) - i64::from(previous_diagonal))
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

#[cfg(test)]
#[path = "chunk_tests.rs"]
mod tests;
