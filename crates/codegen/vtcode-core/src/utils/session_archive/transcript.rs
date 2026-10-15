//! Transcript extraction and normalization for session archive snapshots.

use super::SessionMessage;
use crate::llm::provider::MessageRole;
use crate::utils::session_transcript_norm::{
    format_repeated_summary, normalize_recovery_line, normalized_transcript_key, push_clean_transcript_line,
    should_drop_transcript_line, summarize_tool_block,
};
use std::collections::HashMap;

#[derive(Default)]
struct TranscriptCleaner {
    cleaned: Vec<String>,
    seen_tool_blocks: HashMap<String, SeenToolBlock>,
}

struct SeenToolBlock {
    output_index: usize,
    occurrences: usize,
    original_line: String,
}

pub(in crate::utils) fn progress_transcript_from_recent_messages(recent_messages: &[SessionMessage]) -> Vec<String> {
    let mut transcript = Vec::with_capacity(recent_messages.len());

    for message in recent_messages {
        if !matches!(message.role, MessageRole::User | MessageRole::Assistant) {
            continue;
        }

        let content = message.content.trim();
        let content: &str = content.as_ref();
        if !content.is_empty() && transcript.last().is_none_or(|last: &String| last.as_str() != content) {
            transcript.push(content.to_string());
        }
    }

    clean_transcript_lines(&transcript)
}

pub(in crate::utils) fn clean_transcript_lines(lines: &[String]) -> Vec<String> {
    let mut cleaner = TranscriptCleaner::default();
    let mut index = 0;

    while index < lines.len() {
        index = cleaner.consume_next(lines, index);
    }

    cleaner.finish()
}

impl TranscriptCleaner {
    fn consume_next(&mut self, lines: &[String], index: usize) -> usize {
        let line = lines[index].trim_end();

        if should_reset_tool_dedupe_scope(line) {
            self.seen_tool_blocks.clear();
        }

        if let Some(replacement) = normalize_recovery_line(line) {
            self.push_line(replacement);
            return index + 1;
        }

        if should_drop_transcript_line(line) {
            return index + 1;
        }

        if line.trim_start().starts_with("• ") {
            return self.consume_tool_block(lines, index);
        }

        self.push_line(line.to_string());
        index + 1
    }

    fn consume_tool_block(&mut self, lines: &[String], index: usize) -> usize {
        let (summary, next_index) = summarize_tool_block(lines, index);
        let signature = normalized_transcript_key(&summary);

        if let Some(seen) = self.seen_tool_blocks.get_mut(&signature) {
            seen.occurrences += 1;
            if let Some(existing) = self.cleaned.get_mut(seen.output_index) {
                *existing = format_repeated_summary(&seen.original_line, seen.occurrences);
            }
            return next_index;
        }

        let output_index = self.cleaned.len();
        self.push_line(summary);
        if self.cleaned.len() > output_index {
            self.seen_tool_blocks.insert(
                signature,
                SeenToolBlock {
                    output_index,
                    occurrences: 1,
                    original_line: self.cleaned[output_index].clone(),
                },
            );
        }

        next_index
    }

    fn push_line(&mut self, line: String) {
        push_clean_transcript_line(&mut self.cleaned, line);
    }

    fn finish(mut self) -> Vec<String> {
        while self.cleaned.last().is_some_and(|line| line.is_empty()) {
            self.cleaned.pop();
        }

        self.cleaned
    }
}

fn should_reset_tool_dedupe_scope(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !line.starts_with(' ') && !trimmed.starts_with("• ") && !trimmed.starts_with("[!]")
}
