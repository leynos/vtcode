const OPEN_TAG: &str = "<proposed_plan>";
const CLOSE_TAG: &str = "</proposed_plan>";
const ALT_OPEN_TAG: &str = "<plan>";
const ALT_CLOSE_TAG: &str = "</plan>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProposedPlanExtraction {
    pub stripped_text: String,
    pub plan_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseMode {
    Normal,
    InPlan { close_tag: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanTag {
    Proposed,
    Alternate,
}

impl PlanTag {
    fn open_tag(self) -> &'static str {
        match self {
            Self::Proposed => OPEN_TAG,
            Self::Alternate => ALT_OPEN_TAG,
        }
    }

    fn close_tag(self) -> &'static str {
        match self {
            Self::Proposed => CLOSE_TAG,
            Self::Alternate => ALT_CLOSE_TAG,
        }
    }
}

/// Streaming parser that removes either supported plan block form from
/// assistant-visible text while collecting the plan body.
#[derive(Debug, Default)]
pub(crate) struct ProposedPlanStreamParser {
    mode: Option<ParseMode>,
    pending: String,
    policy_pending: String,
    plan_buffer: String,
    saw_plan_block: bool,
}

impl ProposedPlanStreamParser {
    pub(crate) fn new() -> Self {
        Self {
            mode: Some(ParseMode::Normal),
            pending: String::new(),
            policy_pending: String::new(),
            plan_buffer: String::new(),
            saw_plan_block: false,
        }
    }

    /// Consume streamed text and return only content that should remain visible
    /// to the assistant transcript.
    pub(crate) fn consume(&mut self, chunk: &str) -> String {
        let chunk = self.filter_policy_chunk(chunk, false);
        self.consume_plan_markup(&chunk)
    }

    fn consume_plan_markup(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut visible = String::new();

        loop {
            match self.mode.unwrap_or(ParseMode::Normal) {
                ParseMode::Normal => {
                    if let Some((index, tag)) = find_next_open_tag(&self.pending) {
                        visible.push_str(&self.pending[..index]);
                        self.pending.drain(..index + tag.open_tag().len());
                        self.mode = Some(ParseMode::InPlan { close_tag: tag.close_tag() });
                        self.saw_plan_block = true;
                        continue;
                    }

                    let keep_tail = OPEN_TAG.len().max(ALT_OPEN_TAG.len()).saturating_sub(1).min(self.pending.len());
                    let emit_len = safe_char_boundary(&self.pending, self.pending.len().saturating_sub(keep_tail));
                    visible.push_str(&self.pending[..emit_len]);
                    self.pending.drain(..emit_len);
                    break;
                }
                ParseMode::InPlan { close_tag } => {
                    if let Some(index) = self.pending.find(close_tag) {
                        self.plan_buffer.push_str(&self.pending[..index]);
                        self.pending.drain(..index + close_tag.len());
                        self.mode = Some(ParseMode::Normal);
                        continue;
                    }

                    let keep_tail = close_tag.len().saturating_sub(1).min(self.pending.len());
                    let append_len = safe_char_boundary(&self.pending, self.pending.len().saturating_sub(keep_tail));
                    self.plan_buffer.push_str(&self.pending[..append_len]);
                    self.pending.drain(..append_len);
                    break;
                }
            }
        }

        visible
    }

    /// Finish parsing and return any remaining visible text plus optional plan.
    pub(crate) fn finish(&mut self) -> ProposedPlanExtraction {
        let policy_trailing = self.flush_policy_pending();
        let mut trailing_visible = self.consume_plan_markup(&policy_trailing);
        match self.mode.unwrap_or(ParseMode::Normal) {
            ParseMode::Normal => {
                trailing_visible.push_str(&self.pending);
            }
            ParseMode::InPlan { .. } => {
                // Unterminated block: treat the remainder as plan content.
                self.plan_buffer.push_str(&self.pending);
            }
        }
        self.pending.clear();
        self.mode = Some(ParseMode::Normal);

        ProposedPlanExtraction {
            stripped_text: trailing_visible,
            plan_text: finalize_plan_text(self.saw_plan_block, &self.plan_buffer),
        }
    }

    pub(crate) fn has_unclosed_plan_block(&self) -> bool {
        matches!(self.mode, Some(ParseMode::InPlan { .. })) || has_partial_open_tag(&self.pending)
    }

    fn filter_policy_chunk(&mut self, chunk: &str, flush: bool) -> String {
        self.policy_pending.push_str(chunk);
        filter_policy_text(&mut self.policy_pending, flush)
    }

    fn flush_policy_pending(&mut self) -> String {
        self.filter_policy_chunk("", true)
    }
}

pub(crate) fn extract_proposed_plan(text: &str) -> ProposedPlanExtraction {
    let mut parser = ProposedPlanStreamParser::new();
    let mut stripped = parser.consume(text);
    let trailing = parser.finish();
    stripped.push_str(&trailing.stripped_text);

    ProposedPlanExtraction {
        stripped_text: stripped,
        plan_text: trailing.plan_text,
    }
}

pub(crate) fn extract_any_plan(text: &str) -> ProposedPlanExtraction {
    extract_proposed_plan(text)
}

/// Recovery synthesis accepts one canonical plan block only. Normal planning
/// responses continue to support the legacy `<plan>` alias, but the bounded
/// tool-free recovery contract must be unambiguous before it can be persisted
/// or shown for approval.
pub(crate) fn has_exactly_one_proposed_plan_block(text: &str) -> bool {
    let text = strip_plan_persistence_policy_line(text);
    let Some(open_index) = text.find(OPEN_TAG) else {
        return false;
    };
    let Some(close_index) = text.find(CLOSE_TAG) else {
        return false;
    };
    open_index < close_index
        && text.matches(OPEN_TAG).count() == 1
        && text.matches(CLOSE_TAG).count() == 1
        && !text.contains(ALT_OPEN_TAG)
        && !text.contains(ALT_CLOSE_TAG)
}

pub(crate) fn has_unclosed_plan_block(text: &str) -> bool {
    let mut parser = ProposedPlanStreamParser::new();
    parser.consume(text);
    parser.has_unclosed_plan_block()
}

/// Remove the runtime-owned persistence policy when a provider repeats it as
/// assistant prose around a plan block. The exact line is intentionally
/// bounded and prompt-owned; arbitrary model text is left untouched.
pub(crate) fn strip_plan_persistence_policy_line(text: &str) -> String {
    let stripped = text.replace(diapason_core::prompts::system::PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE, "");
    if stripped == text {
        text.to_string()
    } else {
        stripped.trim().to_string()
    }
}

fn find_next_open_tag(text: &str) -> Option<(usize, PlanTag)> {
    [PlanTag::Proposed, PlanTag::Alternate]
        .into_iter()
        .filter_map(|tag| text.find(tag.open_tag()).map(|index| (index, tag)))
        .min_by_key(|(index, _)| *index)
}

fn has_partial_open_tag(text: &str) -> bool {
    [OPEN_TAG, ALT_OPEN_TAG]
        .into_iter()
        .any(|tag| (1..tag.len()).any(|prefix_len| text.ends_with(&tag[..prefix_len])))
}

fn filter_policy_text(buffer: &mut String, flush: bool) -> String {
    let policy = diapason_core::prompts::system::PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE;
    let mut visible = String::new();

    loop {
        let Some(start) = find_policy_candidate_start(buffer, policy) else {
            visible.push_str(buffer);
            buffer.clear();
            break;
        };

        visible.push_str(&buffer[..start]);
        buffer.drain(..start);
        if buffer.starts_with(policy) {
            buffer.drain(..policy.len());
            continue;
        }
        if !flush && policy.starts_with(buffer.as_str()) {
            break;
        }

        visible.push_str(buffer);
        buffer.clear();
        break;
    }

    visible
}

fn find_policy_candidate_start(text: &str, policy: &str) -> Option<usize> {
    let mut line_start = 0;
    loop {
        let candidate = &text[line_start..];
        if policy.starts_with(candidate) || candidate.starts_with(policy) {
            return Some(line_start);
        }
        let newline = text[line_start..].find('\n')?;
        line_start += newline + 1;
        if line_start >= text.len() {
            return None;
        }
    }
}

fn finalize_plan_text(saw_plan_block: bool, raw: &str) -> Option<String> {
    if !saw_plan_block {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn safe_char_boundary(text: &str, idx: usize) -> usize {
    if idx >= text.len() {
        return text.len();
    }
    if text.is_char_boundary(idx) {
        return idx;
    }
    text.char_indices()
        .take_while(|(pos, _)| *pos < idx)
        .last()
        .map(|(pos, _)| pos)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        ProposedPlanStreamParser, extract_any_plan, extract_proposed_plan, has_exactly_one_proposed_plan_block,
        has_unclosed_plan_block, strip_plan_persistence_policy_line,
    };

    #[test]
    fn extracts_single_proposed_plan_block() {
        let extraction = extract_proposed_plan("Intro\n<proposed_plan>\n- A\n- B\n</proposed_plan>\nOutro");
        assert_eq!(extraction.stripped_text, "Intro\n\nOutro");
        assert_eq!(extraction.plan_text.as_deref(), Some("- A\n- B"));
    }

    #[test]
    fn keeps_text_when_no_plan_block_exists() {
        let extraction = extract_proposed_plan("No plan here");
        assert_eq!(extraction.stripped_text, "No plan here");
        assert!(extraction.plan_text.is_none());
    }

    #[test]
    fn handles_unterminated_plan_block() {
        let extraction = extract_proposed_plan("Before<proposed_plan>\n- Step 1\n- Step 2");
        assert_eq!(extraction.stripped_text, "Before");
        assert_eq!(extraction.plan_text.as_deref(), Some("- Step 1\n- Step 2"));
        assert!(has_unclosed_plan_block("Before<proposed_plan>\n- Step 1\n- Step 2"));
    }

    #[test]
    fn extracts_alternate_plan_block_through_shared_parser() {
        let extraction = extract_any_plan("Intro\n<plan>\n- A\n- B\n</plan>\nOutro");
        assert_eq!(extraction.stripped_text, "Intro\n\nOutro");
        assert_eq!(extraction.plan_text.as_deref(), Some("- A\n- B"));
        assert!(!has_unclosed_plan_block("Intro\n<plan>\n- A\n- B\n</plan>\nOutro"));
    }

    #[test]
    fn handles_unterminated_alternate_plan_block() {
        let extraction = extract_any_plan("Before<plan>\n- Step 1\n- Step 2");
        assert_eq!(extraction.stripped_text, "Before");
        assert_eq!(extraction.plan_text.as_deref(), Some("- Step 1\n- Step 2"));
        assert!(has_unclosed_plan_block("Before<plan>\n- Step 1\n- Step 2"));
        assert!(has_unclosed_plan_block("Before<plan"));
    }

    #[test]
    fn supports_streaming_chunks_with_split_tags() {
        let mut parser = ProposedPlanStreamParser::new();
        let mut visible = String::new();
        visible.push_str(&parser.consume("Intro\n<propo"));
        visible.push_str(&parser.consume("sed_plan>\n- Step"));
        visible.push_str(&parser.consume(" 1\n</proposed_plan>\nOutro"));
        let trailing = parser.finish();
        visible.push_str(&trailing.stripped_text);

        assert_eq!(visible, "Intro\n\nOutro");
        assert_eq!(trailing.plan_text.as_deref(), Some("- Step 1"));
    }

    #[test]
    fn supports_streaming_chunks_with_split_alternate_tags() {
        let mut parser = ProposedPlanStreamParser::new();
        let mut visible = String::new();
        visible.push_str(&parser.consume("Intro\n<pl"));
        visible.push_str(&parser.consume("an>\n- Step"));
        visible.push_str(&parser.consume(" 1\n</plan>\nOutro"));
        let trailing = parser.finish();
        visible.push_str(&trailing.stripped_text);

        assert_eq!(visible, "Intro\n\nOutro");
        assert_eq!(trailing.plan_text.as_deref(), Some("- Step 1"));
    }

    #[test]
    fn handles_multibyte_text_without_panicking() {
        let mut parser = ProposedPlanStreamParser::new();
        let mut visible = String::new();
        visible.push_str(&parser.consume("an’t exit on my"));
        let trailing = parser.finish();
        visible.push_str(&trailing.stripped_text);

        assert_eq!(visible, "an’t exit on my");
        assert!(trailing.plan_text.is_none());
    }

    #[test]
    fn strips_only_the_known_plan_persistence_policy_line() {
        let policy = diapason_core::prompts::system::PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE;
        let text = format!("{policy}\n\nResearch summary");
        assert_eq!(strip_plan_persistence_policy_line(&text), "Research summary");
        assert_eq!(strip_plan_persistence_policy_line("unrelated prose"), "unrelated prose");
    }

    #[test]
    fn policy_echo_does_not_look_like_an_unclosed_plan_block() {
        let policy = diapason_core::prompts::system::PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE;
        let response = format!("{policy}\n<plan>\n- Step 1\n</plan>");
        let extraction = extract_any_plan(&response);
        assert_eq!(extraction.plan_text.as_deref(), Some("- Step 1"));
        assert!(!extraction.stripped_text.contains("Emit exactly one final"));

        let split_at = policy.find("<proposed_plan>").expect("policy tag");
        let mut parser = ProposedPlanStreamParser::new();
        let mut visible = parser.consume(&policy[..split_at]);
        visible.push_str(&parser.consume(&policy[split_at..]));
        visible.push_str(&parser.consume("\n<plan>\n- Step 1\n</plan>"));
        let trailing = parser.finish();
        visible.push_str(&trailing.stripped_text);
        assert!(!visible.contains("Emit exactly one final"));
        assert_eq!(trailing.plan_text.as_deref(), Some("- Step 1"));
    }

    #[test]
    fn recovery_plan_shape_requires_one_canonical_block() {
        assert!(has_exactly_one_proposed_plan_block("<proposed_plan>\n- Step 1\n</proposed_plan>"));
        assert!(!has_exactly_one_proposed_plan_block("<plan>\n- Step 1\n</plan>"));
        assert!(!has_exactly_one_proposed_plan_block(
            "<proposed_plan>\n- A\n</proposed_plan>\n<proposed_plan>\n- B\n</proposed_plan>"
        ));
        assert!(!has_exactly_one_proposed_plan_block("<proposed_plan>\n- Missing close"));
        assert!(!has_exactly_one_proposed_plan_block("<proposed_plan>\n- A\n</proposed_plan>\n</proposed_plan>"));
        assert!(!has_exactly_one_proposed_plan_block("</proposed_plan>\n<proposed_plan>\n- A\n"));
    }
}
