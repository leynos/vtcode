//! Structured line-diff construction and hunk generation.

use hashbrown::HashMap;
use serde::Serialize;
use std::cmp::min;

use super::chunks::{Chunk, compute_diff_chunks};

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

    let formatted = if hunks.is_empty() {
        String::new()
    } else {
        formatter(&hunks, &options)
    };

    DiffBundle { hunks, formatted, is_empty: !has_changes }
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
        let byte = bytes[index];
        if byte != b'\n' && byte != b'\r' {
            index += 1;
            continue;
        }
        let line_end = if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index + 2
        } else {
            index + 1
        };
        lines.push(text[line_start..line_end].to_string());
        line_start = line_end;
        index = line_end;
    }
    if line_start < text.len() {
        lines.push(text[line_start..].to_string());
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
                    let line = old_lines[old_index as usize];
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
                    let line = old_lines[old_index as usize];
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
                    let line = new_lines[new_index as usize];
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
            map.insert(line, ch);
            ch
        };
        encoded.push(token);
    }
    encoded
}

fn next_token_char(counter: &mut u32) -> Option<char> {
    while *counter <= 0x10FFFF {
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
        let slice = &records[start..=end];

        let first = &slice[0];
        let old_start = first.old_line.unwrap_or(first.anchor_old).max(1) as usize;
        let new_start = first.new_line.unwrap_or(first.anchor_new).max(1) as usize;

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

            if let Some(existing_start) = current_start {
                // Close the previous range if this change is beyond its context window
                if idx > current_end {
                    ranges.push((existing_start, current_end));
                    current_start = Some(start);
                    current_end = end;
                } else {
                    if start < existing_start {
                        current_start = Some(start);
                    }
                    if end > current_end {
                        current_end = end;
                    }
                }
            } else {
                current_start = Some(start);
                current_end = end;
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
#[path = "structured_tests.rs"]
mod tests;
