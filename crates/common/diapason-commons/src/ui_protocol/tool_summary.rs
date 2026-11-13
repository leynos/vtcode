//! Data exchanged by compact per-call tool summaries.

use crate::tool_types::CompactStr;

/// Session-local key for a complete tool-output capture.
///
/// This is deliberately a UI protocol identifier. It is not persisted in
/// `ThreadEvent` data and must not be treated as a tool-call identity outside
/// the live terminal session.
pub type ToolOutputId = u64;

/// Presentation metadata for a compact command activity row.
///
/// The complete command output is sent through `RecordToolOutput` separately.
/// Keeping this small metadata object independent means compact rendering can
/// replace a row without truncating, reordering, or otherwise changing the
/// captured stdout, stderr, PTY, or spool-backed transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactActivityMetadata {
    /// Identifier shared by all rows in one contiguous successful-command group.
    pub group_id: u64,
    /// Number of successful commands represented by the row.
    pub command_count: usize,
    /// The command text for a single-command row. Grouped rows leave this empty.
    pub command: Option<CompactStr>,
    /// Number of complete output lines hidden behind the review affordance.
    pub hidden_line_count: usize,
    /// Optional status or artefact text that remains visible in the row.
    pub suffix: Option<CompactStr>,
    /// First complete capture represented by the row, used as the review focus.
    pub review_anchor: Option<ToolOutputId>,
    /// All complete captures represented by the row, in render order.
    ///
    /// `review_anchor` remains the first capture for compatibility with the
    /// click protocol; this list lets the UI re-anchor every member of a
    /// grouped row without guessing from transcript text.
    pub review_anchors: Vec<ToolOutputId>,
}

impl CompactActivityMetadata {
    /// Return the compact row text without the UI-only review affordance.
    pub fn display_text(&self) -> String {
        let mut text = if self.command_count > 1 {
            format!("• Ran {} commands", self.command_count)
        } else {
            format!("• Ran {}", self.command.as_deref().unwrap_or("command"))
        };

        if self.command_count == 1 && self.hidden_line_count > 0 {
            text.push_str(&format!(" · … +{} lines", self.hidden_line_count));
        }
        if let Some(suffix) = self.suffix.as_deref().filter(|suffix| !suffix.is_empty()) {
            text.push_str(" · ");
            text.push_str(suffix);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::CompactActivityMetadata;

    #[test]
    fn compact_activity_display_includes_single_command_output_count() {
        let activity = CompactActivityMetadata {
            group_id: 1,
            command_count: 1,
            command: Some("cargo check".into()),
            hidden_line_count: 3,
            suffix: None,
            review_anchor: Some(7),
            review_anchors: vec![7],
        };

        assert_eq!(activity.display_text(), "• Ran cargo check · … +3 lines");
    }

    #[test]
    fn compact_activity_display_collapses_grouped_commands() {
        let activity = CompactActivityMetadata {
            group_id: 2,
            command_count: 4,
            command: None,
            hidden_line_count: 12,
            suffix: Some("output retained".into()),
            review_anchor: Some(9),
            review_anchors: vec![9],
        };

        assert_eq!(activity.display_text(), "• Ran 4 commands · output retained");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactToolSummaryLine {
    pub kind: CompactToolSummaryLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactToolSummaryLineKind {
    Info,
    Detail,
}
