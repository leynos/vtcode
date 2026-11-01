use std::path::PathBuf;

use thiserror::Error;

/// Errors produced while parsing or applying agent patches.
#[derive(Debug, Error)]
pub enum PatchError {
    #[error("cannot parse empty patch input")]
    EmptyInput,

    #[error("patch does not contain any operations")]
    NoOperations,

    #[error("invalid patch format: {0}")]
    InvalidFormat(String),

    #[error("invalid patch hunk on line {line}: {message}")]
    InvalidHunk { line: usize, message: String },

    #[error("invalid patch operation for '{path}': {reason}")]
    InvalidOperation { path: String, reason: String },

    #[error("invalid path for {operation}: {path} ({reason})")]
    InvalidPath {
        operation: &'static str,
        path: String,
        reason: String,
    },

    #[error("file '{path}' not found for update")]
    MissingFile { path: String },

    #[error(
        "failed to locate context '{context}' in '{path}'. Fix: Use read_file to get exact content, then copy the context lines precisely."
    )]
    ContextNotFound { path: String, context: String },

    #[error(
        "failed to locate expected lines in '{path}':\n{snippet}\n\nFix: The patch context/deletion lines must match file content exactly. Use read_file first, then use those exact lines in your patch."
    )]
    SegmentNotFound { path: String, snippet: String },

    #[error(
        "patch anchor '{anchor}' for '{path}' matches more than one location; rebase assessment requires a unique anchor"
    )]
    AmbiguousAnchor { path: String, anchor: String },

    #[error(
        "patch anchor '{anchor}' for '{path}' does not match exactly; only a whitespace- or Unicode-normalised match exists, which rebase assessment will not use"
    )]
    InexactAnchor { path: String, anchor: String },

    #[error(
        "semantic patch anchor '{anchor}' for '{path}' could not be resolved safely: {reason}\n\nFix: Use a more specific @@ anchor such as a function/class name, or copy exact context lines from read_file."
    )]
    SemanticResolutionFailed {
        path: String,
        anchor: String,
        reason: String,
    },

    #[error(
        "semantic patch anchor '{anchor}' for '{path}' matched {candidate_count} possible locations. Use a more specific @@ anchor or smaller hunk."
    )]
    SemanticAmbiguous {
        path: String,
        anchor: String,
        candidate_count: usize,
    },

    #[error("I/O error while {action} '{path}': {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to generate temporary path for '{path}': {source}")]
    TempPath {
        path: PathBuf,
        #[source]
        source: std::time::SystemTimeError,
    },

    #[error("failed to rollback patch after error ({original}): {rollback}")]
    Recovery {
        original: Box<PatchError>,
        #[source]
        rollback: Box<PatchError>,
    },

    #[error("{original}")]
    RolledBack {
        #[source]
        original: Box<PatchError>,
    },
}

impl PatchError {
    /// Return a fixed, allocation-free label for this error variant.
    ///
    /// Callers that only need to record *why* a patch failed should log this
    /// label instead of the `Display` output. Several variants embed patch text
    /// in their message, and that text must not reach diagnostic streams.
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            PatchError::EmptyInput => "empty_input",
            PatchError::NoOperations => "no_operations",
            PatchError::InvalidFormat(_) => "invalid_format",
            PatchError::InvalidHunk { .. } => "invalid_hunk",
            PatchError::InvalidOperation { .. } => "invalid_operation",
            PatchError::InvalidPath { .. } => "invalid_path",
            PatchError::MissingFile { .. } => "missing_file",
            PatchError::ContextNotFound { .. } => "context_not_found",
            PatchError::SegmentNotFound { .. } => "segment_not_found",
            PatchError::AmbiguousAnchor { .. } => "ambiguous_anchor",
            PatchError::InexactAnchor { .. } => "inexact_anchor",
            PatchError::SemanticResolutionFailed { .. } => "semantic_resolution_failed",
            PatchError::SemanticAmbiguous { .. } => "semantic_ambiguous",
            PatchError::Io { .. } => "io_error",
            PatchError::TempPath { .. } => "temp_path",
            PatchError::Recovery { .. } => "recovery",
            PatchError::RolledBack { .. } => "rolled_back",
        }
    }

    /// Return the byte length of any patch text embedded in this error.
    ///
    /// This is the bounded numeric companion to [`Self::reason_code`]: callers
    /// can report *how much* patch text a failure carried without formatting the
    /// error, whose `Display` would reproduce that text verbatim.
    #[must_use]
    pub fn embedded_text_bytes(&self) -> usize {
        match self {
            PatchError::InvalidFormat(text) => text.len(),
            PatchError::InvalidHunk { message, .. } => message.len(),
            PatchError::InvalidOperation { reason, .. } | PatchError::InvalidPath { reason, .. } => reason.len(),
            PatchError::ContextNotFound { context, .. } => context.len(),
            PatchError::SegmentNotFound { snippet, .. } => snippet.len(),
            PatchError::AmbiguousAnchor { anchor, .. } | PatchError::InexactAnchor { anchor, .. } => anchor.len(),
            PatchError::SemanticResolutionFailed { anchor, reason, .. } => anchor.len() + reason.len(),
            PatchError::SemanticAmbiguous { anchor, .. } => anchor.len(),
            PatchError::Recovery { original, rollback } => {
                original.embedded_text_bytes() + rollback.embedded_text_bytes()
            }
            PatchError::RolledBack { original } => original.embedded_text_bytes(),
            PatchError::EmptyInput
            | PatchError::NoOperations
            | PatchError::MissingFile { .. }
            | PatchError::Io { .. }
            | PatchError::TempPath { .. } => 0,
        }
    }
}
