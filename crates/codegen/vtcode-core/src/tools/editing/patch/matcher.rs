/// How a hunk's anchors are matched against the current file.
///
/// `Sequenced` is the historical apply path: exact, then whitespace-tolerant,
/// then Unicode-normalised matching, with semantic fallback layered on top.
/// `StrictUnique` is used only by rebase assessment and accepts a single
/// byte-exact occurrence, so an ambiguous or merely similar anchor cannot be
/// reported as safely rebasable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchPolicy {
    Sequenced,
    StrictUnique,
}

/// The outcome of a strict, uniqueness-checking anchor search.
pub(crate) enum SeekOutcome {
    /// Exactly one byte-exact occurrence was found at this index.
    Unique(usize),
    /// More than one byte-exact occurrence exists in the search window.
    Ambiguous,
    /// No byte-exact occurrence exists in the search window.
    NotFound,
}

pub(crate) struct PatchContextMatcher<'a> {
    lines: &'a [String],
    /// Normalised form of every file line, computed once at construction so
    /// repeated `seek` calls (and thus repeated patch hunks) don't re-normalise
    /// the whole file on every lookup.
    norm_lines: Vec<String>,
}

impl<'a> PatchContextMatcher<'a> {
    pub(crate) fn new(lines: &'a [String]) -> Self {
        let norm_lines = lines.iter().map(|l| normalise_text(l)).collect();
        Self { lines, norm_lines }
    }

    pub(crate) fn seek(&self, pattern: &[String], start: usize, eof: bool) -> Option<usize> {
        if pattern.is_empty() {
            return Some(start);
        }

        if pattern.len() > self.lines.len() {
            return None;
        }

        let search_start = if eof && self.lines.len() >= pattern.len() {
            self.lines.len().saturating_sub(pattern.len())
        } else {
            start
        };

        let max_start = self.lines.len().saturating_sub(pattern.len());

        for idx in search_start..=max_start {
            if self.lines[idx..idx + pattern.len()] == *pattern {
                return Some(idx);
            }
        }

        for idx in search_start..=max_start {
            let mut ok = true;
            for (offset, pat) in pattern.iter().enumerate() {
                if self.lines[idx + offset].trim_end() != pat.trim_end() {
                    ok = false;
                    break;
                }
            }
            if ok {
                return Some(idx);
            }
        }

        for idx in search_start..=max_start {
            let mut ok = true;
            for (offset, pat) in pattern.iter().enumerate() {
                if self.lines[idx + offset].trim() != pat.trim() {
                    ok = false;
                    break;
                }
            }
            if ok {
                return Some(idx);
            }
        }

        // Compare against the pre-computed normalised file lines (built once in
        // `new`), avoiding re-normalising the whole file on every `seek` call.
        let norm_pattern: Vec<String> = pattern.iter().map(|p| normalise_text(p)).collect();
        let norm_lines = &self.norm_lines;
        for idx in search_start..=max_start {
            let mut ok = true;
            for (offset, norm_pat) in norm_pattern.iter().enumerate() {
                if norm_lines[idx + offset] != *norm_pat {
                    ok = false;
                    break;
                }
            }
            if ok {
                return Some(idx);
            }
        }

        None
    }

    /// Search for a byte-exact occurrence in the same window [`Self::seek`] uses.
    ///
    /// Unlike `seek`, this performs no whitespace, Unicode, or semantic
    /// tolerance and reports ambiguity instead of returning the first hit. It
    /// is the anchor test used by rebase assessment.
    pub(crate) fn seek_exact_unique(&self, pattern: &[String], start: usize, eof: bool) -> SeekOutcome {
        if pattern.is_empty() {
            return SeekOutcome::Unique(start);
        }
        if pattern.len() > self.lines.len() {
            return SeekOutcome::NotFound;
        }

        let search_start = if eof && self.lines.len() >= pattern.len() {
            self.lines.len().saturating_sub(pattern.len())
        } else {
            start
        };
        let max_start = self.lines.len().saturating_sub(pattern.len());
        let mut found = None;

        for idx in search_start..=max_start {
            if self.lines[idx..idx + pattern.len()] == *pattern {
                if found.is_some() {
                    return SeekOutcome::Ambiguous;
                }
                found = Some(idx);
            }
        }

        match found {
            Some(idx) => SeekOutcome::Unique(idx),
            None => SeekOutcome::NotFound,
        }
    }

    /// Report whether a whitespace- or Unicode-tolerant match exists.
    ///
    /// Only meaningful after [`Self::seek_exact_unique`] returned
    /// [`SeekOutcome::NotFound`] for the same window and pattern: `seek` tries
    /// the exact pass first, so any hit in that situation is necessarily
    /// inexact.
    pub(crate) fn has_inexact_match(&self, pattern: &[String], start: usize, eof: bool) -> bool {
        !pattern.is_empty() && self.seek(pattern, start, eof).is_some()
    }
}

pub(crate) fn seek_segment(
    matcher: &PatchContextMatcher,
    old_segment: &mut Vec<String>,
    new_segment: &mut Vec<String>,
    start: usize,
    eof: bool,
) -> Option<usize> {
    let mut found = matcher.seek(old_segment, start, eof);

    if found.is_none() && old_segment.last().is_some_and(|line| line.is_empty()) {
        old_segment.pop();
        if new_segment.last().is_some_and(|line| line.is_empty()) {
            new_segment.pop();
        }
        found = matcher.seek(old_segment, start, eof);
    }

    found
}

pub(crate) fn normalise_text(input: &str) -> String {
    let trimmed = input.trim();
    let mut result = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        let normalised = match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}' | '\u{2007}' | '\u{2008}'
            | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        };
        result.push(normalised);
    }
    result
}
