use std::io::ErrorKind;
use std::path::Path;

use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::super::matcher::{MatchPolicy, PatchContextMatcher, SeekOutcome, seek_segment};
use super::super::semantic::{resolve_semantic_match, semantic_anchor_term};
use super::io::AtomicWriter;
use super::{PatchChunk, PatchError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineEnding {
    LF,
    Crlf,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            LineEnding::LF => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

pub(super) async fn load_file_lines(path: &Path) -> Result<(Vec<String>, bool, LineEnding), PatchError> {
    let file = fs::File::open(path).await.map_err(|err| {
        if err.kind() == ErrorKind::NotFound {
            PatchError::MissingFile { path: path.display().to_string() }
        } else {
            PatchError::Io {
                action: "read",
                path: path.to_path_buf(),
                source: err,
            }
        }
    })?;
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut had_trailing_newline = false;
    let mut line_ending = LineEnding::LF;
    let mut detected_ending = false;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await.map_err(|err| PatchError::Io {
            action: "read",
            path: path.to_path_buf(),
            source: err,
        })?;

        if bytes_read == 0 {
            break;
        }

        if line.ends_with('\n') {
            had_trailing_newline = true;
            if !detected_ending {
                if line.ends_with("\r\n") {
                    line_ending = LineEnding::Crlf;
                }
                detected_ending = true;
            }
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        } else {
            had_trailing_newline = false;
        }

        if line.ends_with('\r') {
            line.pop();
        }

        lines.push(line);
    }

    Ok((lines, had_trailing_newline, line_ending))
}

fn lines_from_content(content: &str) -> (Vec<String>, bool, LineEnding) {
    let had_trailing_newline = content.ends_with('\n');
    let line_ending = if content.contains("\r\n") {
        LineEnding::Crlf
    } else {
        LineEnding::LF
    };
    let lines = content.lines().map(ToOwned::to_owned).collect();
    (lines, had_trailing_newline, line_ending)
}

pub(super) async fn compute_replacements(
    source_path: &Path,
    original_lines: &[String],
    chunks: &[PatchChunk],
    path: &str,
    policy: MatchPolicy,
) -> Result<Vec<(usize, usize, Vec<String>)>, PatchError> {
    if matches!(policy, MatchPolicy::StrictUnique) {
        return compute_replacements_strict(original_lines, chunks, path);
    }

    let matcher = PatchContextMatcher::new(original_lines);
    let mut replacements = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        let mut context_found = true;
        if let Some(hint_line) = chunk.parse_line_number() {
            line_index = hint_line.saturating_sub(1);
        } else if let Some(context) = chunk.change_context() {
            let search_pattern = vec![context.to_string()];
            if let Some(idx) = matcher.seek(&search_pattern, line_index, false) {
                line_index = idx + 1;
            } else {
                context_found = false;
            }
        }

        let (mut old_segment, mut new_segment) = chunk.to_segments();

        if !chunk.has_old_lines() {
            if !context_found && let Some(context) = chunk.change_context() {
                return Err(PatchError::ContextNotFound {
                    path: path.to_string(),
                    context: context.to_string(),
                });
            }
            let insertion_idx = if chunk.change_context().is_some() {
                line_index.min(original_lines.len())
            } else {
                original_lines.len()
            };

            line_index = insertion_idx.saturating_add(new_segment.len());
            replacements.push((insertion_idx, 0, new_segment));
            continue;
        }

        let mut found = if context_found {
            seek_segment(&matcher, &mut old_segment, &mut new_segment, line_index, chunk.is_end_of_file())
        } else {
            None
        };

        if found.is_none() && chunk.change_context().and_then(semantic_anchor_term).is_some() {
            match resolve_semantic_match(
                source_path,
                path,
                original_lines,
                chunk,
                old_segment.clone(),
                new_segment.clone(),
            )
            .await
            {
                Ok(semantic) => {
                    found = Some(semantic.start_idx);
                    old_segment = semantic.old_segment;
                    new_segment = semantic.new_segment;
                }
                Err(PatchError::SemanticResolutionFailed { reason, .. }) if reason.contains("unsupported language") => {
                    // Language not supported by ast-grep (e.g. markdown, yaml).
                    // Fall back to exact text search from the beginning of the file.
                    found = seek_segment(&matcher, &mut old_segment, &mut new_segment, 0, chunk.is_end_of_file());
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(start_idx) = found {
            line_index = start_idx + old_segment.len();
            replacements.push((start_idx, old_segment.len(), new_segment));
        } else {
            if !context_found && let Some(context) = chunk.change_context() {
                return Err(PatchError::ContextNotFound {
                    path: path.to_string(),
                    context: context.to_string(),
                });
            }
            let snippet = if old_segment.is_empty() {
                "<empty>".to_string()
            } else {
                old_segment.join("\n")
            };
            return Err(PatchError::SegmentNotFound { path: path.to_string(), snippet });
        }
    }

    replacements.sort_by_key(|(idx, _, _)| *idx);
    Ok(replacements)
}

/// Compute replacement spans while requiring every anchor to be byte-exact and
/// unique.
///
/// This is the rebase-assessment simulation. It mirrors the sequenced search
/// window and hunk progression, but refuses whitespace/Unicode-tolerant
/// matching, the trailing-empty-line retry, and semantic fallback. Where the
/// sequenced path would silently take the first hit, this reports ambiguity.
fn compute_replacements_strict(
    original_lines: &[String],
    chunks: &[PatchChunk],
    path: &str,
) -> Result<Vec<(usize, usize, Vec<String>)>, PatchError> {
    let matcher = PatchContextMatcher::new(original_lines);
    let mut replacements = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        let mut search_start = line_index;
        if let Some(hint_line) = chunk.parse_line_number() {
            line_index = hint_line.saturating_sub(1);
            search_start = line_index;
        } else if let Some(context) = chunk.change_context() {
            let pattern = vec![context.to_string()];
            match matcher.seek_exact_unique(&pattern, line_index, false) {
                SeekOutcome::Unique(idx) => {
                    line_index = idx + 1;
                    search_start = line_index;
                }
                SeekOutcome::Ambiguous => {
                    return Err(PatchError::AmbiguousAnchor {
                        path: path.to_string(),
                        anchor: context.to_string(),
                    });
                }
                SeekOutcome::NotFound => {
                    return Err(if matcher.has_inexact_match(&pattern, line_index, false) {
                        PatchError::InexactAnchor {
                            path: path.to_string(),
                            anchor: context.to_string(),
                        }
                    } else {
                        PatchError::ContextNotFound {
                            path: path.to_string(),
                            context: context.to_string(),
                        }
                    });
                }
            }
        }

        let (old_segment, new_segment) = chunk.to_segments();

        if !chunk.has_old_lines() {
            let insertion_idx = if chunk.change_context().is_some() {
                line_index.min(original_lines.len())
            } else {
                original_lines.len()
            };

            line_index = insertion_idx.saturating_add(new_segment.len());
            replacements.push((insertion_idx, 0, new_segment));
            continue;
        }

        let anchor = chunk
            .change_context()
            .or_else(|| old_segment.first().map(String::as_str))
            .unwrap_or_default();
        let eof = chunk.is_end_of_file();
        match matcher.seek_exact_unique(&old_segment, search_start, eof) {
            SeekOutcome::Unique(idx) => {
                line_index = idx + old_segment.len();
                replacements.push((idx, old_segment.len(), new_segment));
            }
            SeekOutcome::Ambiguous => {
                return Err(PatchError::AmbiguousAnchor { path: path.to_string(), anchor: anchor.to_string() });
            }
            SeekOutcome::NotFound => {
                return Err(if matcher.has_inexact_match(&old_segment, search_start, eof) {
                    PatchError::InexactAnchor { path: path.to_string(), anchor: anchor.to_string() }
                } else {
                    PatchError::SegmentNotFound {
                        path: path.to_string(),
                        snippet: old_segment.join("\n"),
                    }
                });
            }
        }
    }

    replacements.sort_by_key(|(idx, _, _)| *idx);
    Ok(replacements)
}

pub(super) async fn render_patched_text_from_content(
    source_path: &Path,
    content: &str,
    chunks: &[PatchChunk],
    path: &str,
) -> Result<String, PatchError> {
    let (original_lines, had_trailing_newline, line_ending) = lines_from_content(content);
    let replacements = compute_replacements(source_path, &original_lines, chunks, path, MatchPolicy::Sequenced).await?;
    let ensure_trailing_newline = had_trailing_newline || chunks.iter().any(|chunk| chunk.is_end_of_file());
    Ok(render_patched_content(original_lines, replacements, ensure_trailing_newline, line_ending))
}

/// Render the content a patch would produce under [`MatchPolicy::StrictUnique`].
///
/// Used only by rebase assessment; the apply path keeps the sequenced,
/// fuzzy-tolerant renderer above.
pub(super) async fn render_patched_text_from_content_strict(
    source_path: &Path,
    content: &str,
    chunks: &[PatchChunk],
    path: &str,
) -> Result<String, PatchError> {
    let (original_lines, had_trailing_newline, line_ending) = lines_from_content(content);
    let replacements =
        compute_replacements(source_path, &original_lines, chunks, path, MatchPolicy::StrictUnique).await?;
    let ensure_trailing_newline = had_trailing_newline || chunks.iter().any(|chunk| chunk.is_end_of_file());
    Ok(render_patched_content(original_lines, replacements, ensure_trailing_newline, line_ending))
}

fn render_patched_content(
    original_lines: Vec<String>,
    replacements: Vec<(usize, usize, Vec<String>)>,
    ensure_trailing_newline: bool,
    line_ending: LineEnding,
) -> String {
    let ending = line_ending.as_str();
    let estimated_capacity = original_lines.iter().map(|l| l.len()).sum::<usize>()
        + original_lines.len().saturating_sub(1) * ending.len()
        + ending.len();
    let mut rendered = String::with_capacity(estimated_capacity);
    let mut current_idx = 0usize;
    let mut first = true;

    for (start_idx, old_len, new_segment) in replacements {
        for line in original_lines.iter().take(start_idx).skip(current_idx) {
            if !first {
                rendered.push_str(ending);
            }
            rendered.push_str(line);
            first = false;
        }

        for line in new_segment {
            if !first {
                rendered.push_str(ending);
            }
            rendered.push_str(&line);
            first = false;
        }

        current_idx = start_idx + old_len;
    }

    for line in original_lines.iter().skip(current_idx) {
        if !first {
            rendered.push_str(ending);
        }
        rendered.push_str(line);
        first = false;
    }

    if ensure_trailing_newline && !rendered.ends_with(ending) {
        rendered.push_str(ending);
    }

    rendered
}

pub(super) async fn write_patched_content(
    writer: &mut AtomicWriter,
    original_lines: Vec<String>,
    replacements: Vec<(usize, usize, Vec<String>)>,
    ensure_trailing_newline: bool,
    line_ending: LineEnding,
) -> Result<(), PatchError> {
    let mut current_idx = 0;
    let ending = line_ending.as_str();
    let mut first = true;
    let mut last_line_was_empty = false;

    for (start_idx, old_len, new_segment) in replacements {
        // Write lines before the replacement
        for line in original_lines.iter().take(start_idx).skip(current_idx) {
            if !first {
                writer.write_all(ending.as_bytes()).await?;
            }
            writer.write_all(line.as_bytes()).await?;
            first = false;
            last_line_was_empty = line.is_empty();
        }
        // Write the replacement lines
        for line in new_segment {
            if !first {
                writer.write_all(ending.as_bytes()).await?;
            }
            writer.write_all(line.as_bytes()).await?;
            first = false;
            last_line_was_empty = line.is_empty();
        }
        current_idx = start_idx + old_len;
    }

    // Write remaining lines
    for line in original_lines.iter().skip(current_idx) {
        if !first {
            writer.write_all(ending.as_bytes()).await?;
        }
        writer.write_all(line.as_bytes()).await?;
        first = false;
        last_line_was_empty = line.is_empty();
    }

    // Only add a trailing ending if the content does not already end with one.
    // When the last written line is empty, the separator written before it
    // already produces a trailing ending, mirroring render_patched_content.
    if ensure_trailing_newline && !last_line_was_empty {
        writer.write_all(ending.as_bytes()).await?;
    }

    Ok(())
}
