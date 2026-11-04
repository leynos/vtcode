#![expect(
    clippy::string_slice,
    clippy::cast_possible_truncation,
    reason = "Preview offsets are derived from bounded diff lines and converted to the documented display width."
)]

//! Shared helpers for rendering diff previews.

use crate::diff::{DiffHunk, DiffLineKind};
use crate::diff_paths::{
    format_start_only_hunk_header, is_diff_addition_line, is_diff_deletion_line, parse_hunk_starts,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiffChangeCounts {
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffDisplayKind {
    Metadata,
    HunkHeader,
    Context,
    Addition,
    Deletion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffDisplayLine {
    pub kind: DiffDisplayKind,
    pub line_number: Option<u32>,
    pub text: String,
}

impl DiffDisplayKind {
    /// Whether this kind carries diff body content (context, addition, or
    /// deletion) rather than metadata or a hunk header.
    pub fn is_diff(self) -> bool {
        matches!(self, Self::Context | Self::Addition | Self::Deletion)
    }
}

impl DiffDisplayLine {
    /// Whether this line carries diff content, without re-parsing its text.
    pub fn is_diff(&self) -> bool {
        self.kind.is_diff()
    }

    /// Render with an unambiguous gutter: `marker + number + │ + content`.
    ///
    /// The `│` separator keeps markdown bullets (`- foo`) and list markers
    /// visually distinct from the diff marker (`+`/`-`/` `).
    pub fn numbered_text(&self, line_number_width: usize) -> String {
        match self.kind {
            DiffDisplayKind::Metadata | DiffDisplayKind::HunkHeader => self.text.clone(),
            DiffDisplayKind::Addition => {
                format!("+{:>line_number_width$} │ {}", self.line_number.unwrap_or_default(), self.text)
            }
            DiffDisplayKind::Deletion => {
                format!("-{:>line_number_width$} │ {}", self.line_number.unwrap_or_default(), self.text)
            }
            DiffDisplayKind::Context => {
                format!(" {:>line_number_width$} │ {}", self.line_number.unwrap_or_default(), self.text)
            }
        }
    }
}

impl DiffChangeCounts {
    pub fn total(self) -> usize {
        self.additions + self.deletions
    }
}

pub fn count_diff_changes(hunks: &[DiffHunk]) -> DiffChangeCounts {
    let mut counts = DiffChangeCounts::default();

    for hunk in hunks {
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Addition => counts.additions += 1,
                DiffLineKind::Deletion => counts.deletions += 1,
                DiffLineKind::Context => {}
            }
        }
    }

    counts
}

pub fn display_lines_from_hunks(hunks: &[DiffHunk]) -> Vec<DiffDisplayLine> {
    // Each hunk contributes 1 header + its lines; pre-size to avoid reallocations
    // on large diffs (the count is exact, so no over-allocation).
    let total = hunks.iter().map(|h| 1 + h.lines.len()).sum();
    let mut lines = Vec::with_capacity(total);

    for hunk in hunks {
        lines.push(DiffDisplayLine {
            kind: DiffDisplayKind::HunkHeader,
            line_number: None,
            text: format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start),
        });

        for line in &hunk.lines {
            lines.push(display_line_from_diff_line(line));
        }
    }

    lines
}

pub fn display_lines_from_unified_diff(diff_content: &str) -> Vec<DiffDisplayLine> {
    // Upper-bound the capacity to the line count — the double scan is cheap
    // (L1-bound byte search) and avoids 10+ reallocations on large diffs.
    let mut lines = Vec::with_capacity(diff_content.lines().count());
    let mut old_line_no = 0u32;
    let mut new_line_no = 0u32;
    let mut in_hunk = false;

    for line in diff_content.lines() {
        if let Some((old_start, new_start)) = parse_hunk_starts(line) {
            old_line_no = old_start as u32;
            new_line_no = new_start as u32;
            in_hunk = true;
            lines.push(DiffDisplayLine {
                kind: DiffDisplayKind::HunkHeader,
                line_number: None,
                text: format_start_only_hunk_header(line).unwrap_or_else(|| format!("@@ -{old_start} +{new_start} @@")),
            });
            continue;
        }

        if !in_hunk {
            lines.push(DiffDisplayLine {
                kind: DiffDisplayKind::Metadata,
                line_number: None,
                text: line.to_string(),
            });
            continue;
        }

        if is_diff_addition_line(line) {
            lines.push(DiffDisplayLine {
                kind: DiffDisplayKind::Addition,
                line_number: Some(new_line_no),
                text: line[1..].to_string(),
            });
            new_line_no = new_line_no.saturating_add(1);
            continue;
        }

        if is_diff_deletion_line(line) {
            lines.push(DiffDisplayLine {
                kind: DiffDisplayKind::Deletion,
                line_number: Some(old_line_no),
                text: line[1..].to_string(),
            });
            old_line_no = old_line_no.saturating_add(1);
            continue;
        }

        if let Some(context_line) = line.strip_prefix(' ') {
            lines.push(DiffDisplayLine {
                kind: DiffDisplayKind::Context,
                line_number: Some(new_line_no),
                text: context_line.to_string(),
            });
            old_line_no = old_line_no.saturating_add(1);
            new_line_no = new_line_no.saturating_add(1);
            continue;
        }

        if let Some(omitted) = parse_omitted_line_count(line) {
            old_line_no = old_line_no.saturating_add(omitted);
            new_line_no = new_line_no.saturating_add(omitted);
            lines.push(DiffDisplayLine {
                kind: DiffDisplayKind::Metadata,
                line_number: None,
                text: line.to_string(),
            });
            continue;
        }

        lines.push(DiffDisplayLine {
            kind: DiffDisplayKind::Metadata,
            line_number: None,
            text: line.to_string(),
        });
    }

    lines
}

pub fn diff_display_line_number_width(lines: &[DiffDisplayLine]) -> usize {
    let max_digits = lines
        .iter()
        .filter_map(|line| line.line_number)
        .map(digit_count)
        .max()
        .unwrap_or(4);
    max_digits.clamp(5, 6)
}

fn digit_count(mut value: u32) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

pub fn format_numbered_unified_diff(diff_content: &str) -> Vec<String> {
    let display_lines = display_lines_from_unified_diff(diff_content);
    let width = diff_display_line_number_width(&display_lines);
    display_lines.into_iter().map(|line| line.numbered_text(width)).collect()
}

/// Parse the number of omitted lines from a condensation marker such as
/// `"... 12 lines omitted ..."`.
fn parse_omitted_line_count(line: &str) -> Option<u32> {
    let trimmed = line.trim();
    let after = trimmed.strip_prefix("...")?;
    let after = after.trim_start();
    let digits_end = after.find(|ch: char| !ch.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    after[..digits_end].parse().ok()
}

fn display_line_from_diff_line(line: &crate::diff::DiffLine) -> DiffDisplayLine {
    let text = line.text.trim_end_matches('\n').to_string();
    match line.kind {
        DiffLineKind::Context => DiffDisplayLine {
            kind: DiffDisplayKind::Context,
            line_number: line.new_line,
            text,
        },
        DiffLineKind::Addition => DiffDisplayLine {
            kind: DiffDisplayKind::Addition,
            line_number: line.new_line,
            text,
        },
        DiffLineKind::Deletion => DiffDisplayLine {
            kind: DiffDisplayKind::Deletion,
            line_number: line.old_line,
            text,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, DiffLineKind};

    #[test]
    fn counts_diff_changes_from_hunks() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    old_line: Some(1),
                    new_line: Some(1),
                    text: "same\n".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Deletion,
                    old_line: Some(2),
                    new_line: None,
                    text: "old\n".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(2),
                    text: "new\n".to_string(),
                },
            ],
        }];

        let counts = count_diff_changes(&hunks);
        assert_eq!(counts.additions, 1);
        assert_eq!(counts.deletions, 1);
        assert_eq!(counts.total(), 2);
    }

    #[test]
    fn formats_numbered_unified_diff_with_start_only_headers() {
        let diff = "\
diff --git a/file.txt b/file.txt
@@ -10,2 +10,2 @@
-old
+new
 context
";

        let lines = format_numbered_unified_diff(diff);
        assert_eq!(lines[0], "diff --git a/file.txt b/file.txt");
        assert!(lines.iter().any(|line| line == "@@ -10 +10 @@"));
        assert!(lines.iter().any(|line| line.starts_with("-   10 │ old")));
        assert!(lines.iter().any(|line| line.starts_with("+   10 │ new")));
        assert!(lines.iter().any(|line| line.starts_with("    11 │ context")));
    }

    #[test]
    fn numbered_text_uses_pipe_separator_for_markdown_bullets() {
        let line = DiffDisplayLine {
            kind: DiffDisplayKind::Addition,
            line_number: Some(53),
            text: "- **Agent-first by design**: prose".to_string(),
        };
        assert_eq!(line.numbered_text(5), "+   53 │ - **Agent-first by design**: prose");
    }

    #[test]
    fn display_lines_from_hunks_preserves_semantics() {
        let hunks = vec![DiffHunk {
            old_start: 10,
            old_lines: 2,
            new_start: 10,
            new_lines: 2,
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Deletion,
                    old_line: Some(10),
                    new_line: None,
                    text: "old\n".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(10),
                    text: "new\n".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    old_line: Some(11),
                    new_line: Some(11),
                    text: "same\n".to_string(),
                },
            ],
        }];

        let lines = display_lines_from_hunks(&hunks);
        assert_eq!(lines[0].kind, DiffDisplayKind::HunkHeader);
        assert_eq!(lines[0].text, "@@ -10 +10 @@");
        assert_eq!(lines[1].kind, DiffDisplayKind::Deletion);
        assert_eq!(lines[1].line_number, Some(10));
        assert_eq!(lines[1].text, "old");
        assert_eq!(lines[2].kind, DiffDisplayKind::Addition);
        assert_eq!(lines[2].line_number, Some(10));
        assert_eq!(lines[3].kind, DiffDisplayKind::Context);
        assert_eq!(lines[3].line_number, Some(11));
    }

    #[test]
    fn diff_display_line_number_width_tracks_max_digits() {
        let lines = vec![
            DiffDisplayLine {
                kind: DiffDisplayKind::Addition,
                line_number: Some(99),
                text: "let a = 1;".to_string(),
            },
            DiffDisplayLine {
                kind: DiffDisplayKind::Context,
                line_number: Some(10_420),
                text: "let b = 2;".to_string(),
            },
        ];

        assert_eq!(diff_display_line_number_width(&lines), 5);
    }

    #[test]
    fn preserves_plain_text_when_not_diff() {
        let lines = format_numbered_unified_diff("plain text output");
        assert_eq!(lines, vec!["plain text output".to_string()]);
    }

    #[test]
    fn is_diff_discriminates_content_lines() {
        let diff = "\
diff --git a/file.txt b/file.txt
@@ -1 +1 @@
-old
+new
 context
";
        let lines = display_lines_from_unified_diff(diff);
        assert_eq!(lines[0].kind, DiffDisplayKind::Metadata);
        assert!(!lines[0].is_diff());
        assert_eq!(lines[1].kind, DiffDisplayKind::HunkHeader);
        assert!(!lines[1].is_diff());
        assert!(lines[2].is_diff());
        assert!(lines[3].is_diff());
        assert!(lines[4].is_diff());
    }

    #[test]
    fn omitted_marker_advances_both_counters() {
        let diff = "\
@@ -1,5 +1,5 @@
-one
... 3 lines omitted ...
 old tail
";

        let lines = display_lines_from_unified_diff(diff);
        assert_eq!(lines[1].kind, DiffDisplayKind::Deletion);
        assert_eq!(lines[1].line_number, Some(1));
        assert_eq!(lines[2].kind, DiffDisplayKind::Metadata);
        assert_eq!(lines[2].line_number, None);
        assert_eq!(lines[3].kind, DiffDisplayKind::Context);
        assert_eq!(lines[3].line_number, Some(4));
    }

    #[test]
    fn diff_display_line_number_width_clamps_to_bounds() {
        let small = vec![DiffDisplayLine {
            kind: DiffDisplayKind::Context,
            line_number: Some(1),
            text: "text".to_string(),
        }];
        assert_eq!(diff_display_line_number_width(&small), 5);

        let large = vec![DiffDisplayLine {
            kind: DiffDisplayKind::Context,
            line_number: Some(100_000),
            text: "text".to_string(),
        }];
        assert_eq!(diff_display_line_number_width(&large), 6);
    }

    #[test]
    fn metadata_lines_stay_metadata_after_hunk() {
        let diff = "\
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
";

        let lines = display_lines_from_unified_diff(diff);
        assert_eq!(lines[2].kind, DiffDisplayKind::Metadata);
        assert_eq!(lines[2].line_number, None);
        assert_eq!(lines[3].kind, DiffDisplayKind::Addition);
        assert_eq!(lines[3].line_number, Some(1));
    }
}
