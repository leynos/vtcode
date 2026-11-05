//! Loader for fenced examples marked as tested in published documentation.
//!
//! A user-facing document marks an executable example with
//! `<!-- tested-example: <identifier> -->` on the line immediately before its
//! fence. This module extracts such a fence so a test runs the shipped text
//! rather than a copied fixture, which is what keeps a documented example from
//! drifting away from the behaviour it claims.
//!
//! It is the seed of the shared loader the documentation style guide requires:
//! integration tests use it today, and the remaining repo-wide enforcement
//! (rejecting unmarked or duplicate fences across every user-facing document)
//! is tracked separately.

#![allow(
    dead_code,
    reason = "Shared by integration test binaries; each one uses a subset of the loader."
)]

use std::path::{Path, PathBuf};

/// A fenced example extracted from a published document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentedExample {
    /// The stable identifier taken from the marker comment.
    pub identifier: String,
    /// The fence's info string, such as `json`.
    pub language: String,
    /// The fence body, without the surrounding fence lines.
    pub body: String,
}

/// The workspace root, derived from this crate's manifest directory.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Read a document relative to the workspace root and extract one example.
pub fn load_example(relative_path: &str, identifier: &str) -> Result<DocumentedExample, String> {
    let path = workspace_root().join(relative_path);
    let markdown =
        std::fs::read_to_string(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    extract_example(&markdown, identifier)
}

/// Extract the fence marked `identifier` from `markdown`.
///
/// The marker must directly precede the fence. A missing marker, a duplicate
/// identifier, a marker without a fence, and an unterminated fence are all
/// errors, so a document cannot silently stop being exercised.
pub fn extract_example(markdown: &str, identifier: &str) -> Result<DocumentedExample, String> {
    let marker = format!("<!-- tested-example: {identifier} -->");
    let mut lines = markdown.lines().enumerate();
    let mut found: Option<DocumentedExample> = None;

    while let Some((index, line)) = lines.next() {
        if line.trim() != marker {
            continue;
        }
        if found.is_some() {
            return Err(format!("duplicate tested-example identifier {identifier:?}"));
        }

        // A blank line between the marker and its fence is ordinary Markdown
        // layout, so skip it; any other intervening content means the marker
        // does not belong to the next fence.
        let (fence_index, fence_line) = loop {
            match lines.next() {
                Some((_, line)) if line.trim().is_empty() => continue,
                Some((index, line)) => break (index, line),
                None => {
                    return Err(format!("marker {identifier:?} at line {} is not followed by a fence", index + 1));
                }
            }
        };
        let language = fence_line
            .trim()
            .strip_prefix("```")
            .ok_or_else(|| format!("marker {identifier:?} at line {} is not followed by a fence", index + 1))?;

        let mut body = String::new();
        let mut terminated = false;
        for (_, body_line) in lines.by_ref() {
            if body_line.trim() == "```" {
                terminated = true;
                break;
            }
            body.push_str(body_line);
            body.push('\n');
        }
        if !terminated {
            return Err(format!("fence for {identifier:?} at line {} is unterminated", fence_index + 1));
        }

        found = Some(DocumentedExample {
            identifier: identifier.to_string(),
            language: language.trim().to_string(),
            body,
        });
    }

    found.ok_or_else(|| format!("no tested-example marker for {identifier:?}"))
}

#[cfg(test)]
mod tests {
    //! The loader's contract: a marked fence round-trips, and every malformed
    //! shape is rejected rather than silently skipped.

    use super::extract_example;

    #[test]
    fn extracts_the_marked_fence_without_its_markers() {
        let markdown = "Text.\n\n<!-- tested-example: sample -->\n\n```json\n{\"a\": 1}\n```\n\nAfter.\n";
        let example = extract_example(markdown, "sample").expect("marked fence must load");
        assert_eq!(example.identifier, "sample");
        assert_eq!(example.language, "json");
        assert_eq!(example.body, "{\"a\": 1}\n");
    }

    #[test]
    fn rejects_a_missing_marker() {
        let error = extract_example("```json\n{}\n```\n", "sample").expect_err("an unmarked fence must not load");
        assert!(error.contains("no tested-example marker"), "{error}");
    }

    #[test]
    fn rejects_a_duplicate_identifier() {
        let markdown = "<!-- tested-example: sample -->\n```json\n{}\n```\n\
                        <!-- tested-example: sample -->\n```json\n{}\n```\n";
        let error = extract_example(markdown, "sample").expect_err("a duplicate identifier must not load");
        assert!(error.contains("duplicate tested-example"), "{error}");
    }

    #[test]
    fn rejects_a_marker_without_a_fence() {
        let error = extract_example("<!-- tested-example: sample -->\n", "sample")
            .expect_err("a marker without a fence must not load");
        assert!(error.contains("not followed by a fence"), "{error}");
    }

    #[test]
    fn rejects_an_unterminated_fence() {
        let error = extract_example("<!-- tested-example: sample -->\n```json\n{}\n", "sample")
            .expect_err("an unterminated fence must not load");
        assert!(error.contains("unterminated"), "{error}");
    }
}
