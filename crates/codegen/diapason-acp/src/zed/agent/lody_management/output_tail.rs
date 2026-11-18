//! Bounded extraction of a foreground subagent's transcript tail.
//!
//! A foreground output poll reads the whole thread snapshot, but only needs its
//! final few lines. Joining every message and then taking a tail would allocate
//! the complete transcript on every poll, so this module collects just the
//! newest messages that cover the requested tail.

use diapason_core::llm::provider::Message;
use diapason_core::subagents::extract_tail_lines;

/// Default number of output lines returned when a client omits `tail`.
///
/// This is a transport-contract default: it belongs to the ACP adapter, not to
/// the domain controller that only applies a caller-supplied bound.
const DEFAULT_TASK_OUTPUT_TAIL_LINES: usize = 200;

/// Hard upper bound on a client-requested output tail.
///
/// The bound keeps a single ACP response bounded; the domain controller has no
/// opinion about it.
const MAX_TASK_OUTPUT_TAIL_LINES: usize = 10_000;

/// Resolves a requested output-tail size against the ACP transport contract.
///
/// `None` selects [`DEFAULT_TASK_OUTPUT_TAIL_LINES`]; values above
/// [`MAX_TASK_OUTPUT_TAIL_LINES`] are rejected so one response cannot grow
/// without bound.
pub(super) fn normalize_output_tail_lines(max_lines: Option<usize>) -> Result<usize, String> {
    let max_lines = max_lines.unwrap_or(DEFAULT_TASK_OUTPUT_TAIL_LINES);
    if max_lines > MAX_TASK_OUTPUT_TAIL_LINES {
        return Err(format!("output tail exceeds the hard maximum of {MAX_TASK_OUTPUT_TAIL_LINES} lines"));
    }
    Ok(max_lines)
}

/// Returns the last `max_lines` lines of the foreground message transcript.
///
/// The result is identical to joining every non-empty message text with `"\n"`
/// and passing that to [`extract_tail_lines`], but the walk stops as soon as
/// the collected suffix covers `max_lines`.
///
/// Messages are visited newest to oldest. A message contributes its own
/// `lines()` count, and prepending it adds one more line when it ends with a
/// newline, because the trailing newline and the joining separator then produce
/// an empty line. That count is exact, so the collected suffix cannot fall
/// short of the requested tail; once it reaches `max_lines`, its own tail is
/// the transcript's tail.
pub(super) fn foreground_output_tail(messages: &[Message], max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let mut collected: Vec<String> = Vec::new();
    let mut collected_lines = 0usize;
    for message in messages.iter().rev() {
        let text = message.content.as_text();
        if text.is_empty() {
            continue;
        }
        if text.ends_with('\n') && !collected.is_empty() {
            collected_lines += 1;
        }
        collected_lines += text.lines().count();
        collected.push(text.into_owned());
        if collected_lines >= max_lines {
            break;
        }
    }
    if collected.is_empty() {
        return String::new();
    }
    collected.reverse();
    let transcript = collected.join("\n");
    extract_tail_lines(&transcript, max_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diapason_core::llm::provider::MessageContent;
    use proptest::prelude::*;

    fn message(text: &str) -> Message {
        Message {
            content: MessageContent::Text(text.to_string()),
            ..Message::default()
        }
    }

    fn transcript(texts: &[&str]) -> Vec<Message> {
        texts.iter().map(|text| message(text)).collect()
    }

    #[test]
    fn zero_line_request_returns_nothing() {
        assert_eq!(foreground_output_tail(&transcript(&["one", "two"]), 0), "");
    }

    #[test]
    fn fewer_messages_than_the_requested_tail_returns_all_of_them() {
        assert_eq!(foreground_output_tail(&transcript(&["one", "two"]), 10), "one\ntwo");
    }

    #[test]
    fn multi_line_messages_keep_their_original_order() {
        let messages = transcript(&["alpha\nbeta", "gamma\ndelta"]);
        assert_eq!(foreground_output_tail(&messages, 3), "beta\ngamma\ndelta");
    }

    #[test]
    fn a_message_ending_with_a_newline_contributes_its_empty_line() {
        let messages = transcript(&["a\nb\n", "c"]);
        assert_eq!(foreground_output_tail(&messages, 2), "\nc");
    }

    proptest! {
        /// The bounded walk agrees with bounding the whole transcript.
        ///
        /// For any message sequence and line limit, the returned tail is a
        /// suffix of the whole-transcript extraction, never carries more lines
        /// than the limit, and an oversized limit returns the whole output.
        #[test]
        fn bounded_tail_is_a_suffix_within_the_limit(
            texts in prop::collection::vec("[a-z\n]{0,12}", 0..=6),
            max_lines in 0usize..=20,
        ) {
            let messages: Vec<Message> = texts.iter().map(|text| message(text)).collect();
            let joined = texts
                .iter()
                .filter(|text| !text.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let whole = extract_tail_lines(&joined, usize::MAX);
            // Count the source text's lines, not the whole extraction's: the
            // extraction joins lines with `\n`, so a transcript that ended with
            // a newline loses its final empty line and reports one line fewer
            // than the limit has to cover to include every line.
            let source_lines = joined.lines().count();
            let tail = foreground_output_tail(&messages, max_lines);

            prop_assert!(
                whole.ends_with(&tail),
                "tail {:?} must be a suffix of the whole output {:?}",
                tail,
                whole,
            );
            prop_assert!(
                tail.lines().count() <= max_lines,
                "tail {:?} exceeds the limit of {} lines",
                tail,
                max_lines,
            );
            if max_lines >= source_lines {
                prop_assert_eq!(
                    &tail,
                    &whole,
                    "an oversized limit must return the whole output",
                );
            }
        }
    }

    #[test]
    fn matches_joining_the_whole_transcript() {
        let cases: &[&[&str]] = &[
            &[""],
            &["one"],
            &["one", "", "two"],
            &["alpha\nbeta", "gamma"],
            &["first", "second\n"],
            &["a\nb\n", "c", "d\ne"],
            &["tail\n", "", "middle\nline\n", "head"],
        ];
        for texts in cases {
            let messages = transcript(texts);
            let joined = texts
                .iter()
                .filter(|text| !text.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            for max_lines in 0..=8 {
                assert_eq!(
                    foreground_output_tail(&messages, max_lines),
                    extract_tail_lines(&joined, max_lines),
                    "messages {texts:?} with max_lines {max_lines}"
                );
            }
        }
    }
}
