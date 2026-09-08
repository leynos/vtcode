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
    let old_rest = &old[prefix_byte_len..];
    let new_rest = &new[prefix_byte_len..];

    let suffix_byte_len: usize = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(o, n)| o == n)
        .map(|(c, _)| c.len_utf8())
        .sum();

    let old_middle_end = old_rest.len() - suffix_byte_len;
    let new_middle_end = new_rest.len() - suffix_byte_len;

    let old_middle = &old_rest[..old_middle_end];
    let new_middle = &new_rest[..new_middle_end];

    let mut result = Vec::with_capacity(old_middle.len() + new_middle.len());

    // Add common prefix
    if prefix_byte_len > 0 {
        result.push(Chunk::Equal(&old[..prefix_byte_len]));
    }

    // Compute optimal diff for the middle section
    if !old_middle.is_empty() || !new_middle.is_empty() {
        let old_chars: Vec<char> = old_middle.chars().collect();
        let new_chars: Vec<char> = new_middle.chars().collect();
        let old_byte_starts: Vec<usize> = old_middle.char_indices().map(|(idx, _)| idx).collect();
        let new_byte_starts: Vec<usize> = new_middle.char_indices().map(|(idx, _)| idx).collect();
        let edits = myers_diff(&old_chars, &new_chars);

        let mut old_pos = 0;
        let mut new_pos = 0;
        // Track the start of a consecutive Equal run so we can emit a single
        // Chunk::Equal for the whole run (instead of one per character).
        let mut equal_run_start: Option<usize> = None;

        for edit in edits {
            match edit {
                Edit::Equal => {
                    if equal_run_start.is_none() {
                        equal_run_start = Some(old_pos);
                    }
                    old_pos += 1;
                    new_pos += 1;
                }
                Edit::Delete => {
                    // Flush any accumulated equal run before emitting a Delete
                    if let Some(start) = equal_run_start.take() {
                        let byte_start = old_byte_starts[start];
                        let byte_end = old_byte_starts[old_pos];
                        if byte_start < byte_end {
                            result.push(Chunk::Equal(&old_middle[byte_start..byte_end]));
                        }
                    }
                    let Some(ch) = old_chars.get(old_pos).copied() else {
                        break;
                    };
                    let Some(byte_start) = old_byte_starts.get(old_pos).copied() else {
                        break;
                    };
                    let byte_end = byte_start + ch.len_utf8();
                    result.push(Chunk::Delete(&old_middle[byte_start..byte_end]));
                    old_pos += 1;
                }
                Edit::Insert => {
                    // Flush any accumulated equal run before emitting an Insert.
                    // old_pos may equal old_byte_starts.len() when the equal run
                    // reaches the end of old_middle, so use old_middle.len() as fallback.
                    if let Some(start) = equal_run_start.take() {
                        let byte_start = old_byte_starts[start];
                        let byte_end = if old_pos < old_byte_starts.len() {
                            old_byte_starts[old_pos]
                        } else {
                            old_middle.len()
                        };
                        if byte_start < byte_end {
                            result.push(Chunk::Equal(&old_middle[byte_start..byte_end]));
                        }
                    }
                    let Some(ch) = new_chars.get(new_pos).copied() else {
                        break;
                    };
                    let Some(byte_start) = new_byte_starts.get(new_pos).copied() else {
                        break;
                    };
                    let byte_end = byte_start + ch.len_utf8();
                    result.push(Chunk::Insert(&new_middle[byte_start..byte_end]));
                    new_pos += 1;
                }
            }
        }
        // Flush any trailing equal run
        if let Some(start) = equal_run_start.take() {
            let byte_start = old_byte_starts[start];
            let byte_end = old_middle.len();
            if byte_start < byte_end {
                result.push(Chunk::Equal(&old_middle[byte_start..byte_end]));
            }
        }
    }

    // Add common suffix
    if suffix_byte_len > 0 {
        result.push(Chunk::Equal(&old[old.len() - suffix_byte_len..]));
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
fn advance_matching(old: &[char], new: &[char], mut x: usize, mut y: usize) -> (usize, usize) {
    while x < old.len() && y < new.len() && old[x] == new[y] {
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

#[allow(
    clippy::cast_sign_loss,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
fn myers_diff(old: &[char], new: &[char]) -> Vec<Edit> {
    let n = old.len();
    let m = new.len();

    if n == 0 {
        return vec![Edit::Insert; m];
    }
    if m == 0 {
        return vec![Edit::Delete; n];
    }

    let max_d = n.saturating_add(m).min(i32::MAX as usize);
    let max_d_i32 = max_d as i32;
    let mut v = vec![0; 2 * max_d + 1];
    let mut v_index = vec![0usize; (max_d + 1) * (2 * max_d + 1)];
    let row_len = 2 * max_d + 1;

    v[max_d] = 0;

    for d in 0..=max_d {
        let d_i32 = d as i32;
        let row_start = d * row_len;
        for k in (-d_i32..=d_i32).step_by(2) {
            let k_idx = (k + max_d_i32) as usize;

            let x = if k == -d_i32 || (k != d_i32 && v[k_idx - 1] < v[k_idx + 1]) {
                v[k_idx + 1]
            } else {
                v[k_idx - 1] + 1
            };

            let mut x = x;
            let mut y = (x as i32 - k) as usize;

            (x, y) = advance_matching(old, new, x, y);

            v[k_idx] = x;
            v_index[row_start + k_idx] = x;

            if x >= n && y >= m {
                return backtrack_myers(old, new, &v_index, d, k, max_d);
            }
        }
    }

    vec![]
}

#[allow(
    clippy::cast_sign_loss,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
fn backtrack_myers(old: &[char], new: &[char], v_index: &[usize], d: usize, mut k: i32, max_d: usize) -> Vec<Edit> {
    let mut edits = Vec::with_capacity(old.len() + new.len());
    let mut x = old.len();
    let mut y = new.len();
    let max_d_i32 = max_d as i32;
    let row_len = 2 * max_d + 1;

    for cur_d in (0..=d).rev() {
        if cur_d == 0 {
            while x > 0 && y > 0 {
                edits.push(Edit::Equal);
                x -= 1;
                y -= 1;
            }
            break;
        }

        let k_idx = (k + max_d_i32) as usize;
        let prev_row_start = (cur_d - 1) * row_len;

        let cur_d_i32 = cur_d as i32;
        let prev_k = if k == cur_d_i32.wrapping_neg()
            || (k != cur_d_i32 && v_index[prev_row_start + k_idx - 1] < v_index[prev_row_start + k_idx + 1])
        {
            k + 1
        } else {
            k - 1
        };

        let prev_k_idx = (prev_k + max_d_i32) as usize;
        let prev_x_val = v_index[prev_row_start + prev_k_idx];
        let prev_y = (prev_x_val as i32 - prev_k) as usize;

        let (move_x, move_y) = if prev_k == k + 1 {
            (prev_x_val, prev_y + 1)
        } else {
            (prev_x_val + 1, prev_y)
        };

        (x, y) = backtrack_equal_run(x, y, move_x, move_y, &mut edits);

        if prev_k == k + 1 {
            edits.push(Edit::Insert);
            y -= 1;
        } else {
            edits.push(Edit::Delete);
            x -= 1;
        }

        k = prev_k;
    }

    edits.reverse();
    edits
}

#[cfg(test)]
#[path = "chunk_tests.rs"]
mod tests;
