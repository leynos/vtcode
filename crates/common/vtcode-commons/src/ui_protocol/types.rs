//! Pure data types with no dependencies beyond `std`.

use num_traits::ToPrimitive;

use crate::utils::saturating_float_to_u8;

/// Message kind tag for inline transcript lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineMessageKind {
    Agent,
    Error,
    Info,
    Policy,
    Pty,
    Tool,
    User,
    Warning,
}

/// A single slash-command entry for the suggestion palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandItem {
    pub name: String,
    pub description: String,
}

impl SlashCommandItem {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }
}

/// Search configuration for a list overlay.
#[derive(Clone, Debug)]
pub struct InlineListSearchConfig {
    pub label: String,
    pub placeholder: Option<String>,
}

/// Configuration for a secure (masked) prompt input.
#[derive(Clone, Debug)]
pub struct SecurePromptConfig {
    pub label: String,
    /// Optional placeholder shown when input is empty.
    pub placeholder: Option<String>,
    /// Whether the input should be masked (e.g., API keys).
    pub mask_input: bool,
}

/// Standalone surface preference for selecting inline vs alternate rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSurface {
    Auto,
    Alternate,
    #[default]
    Inline,
}

/// Standalone keyboard protocol settings for terminal key event enhancements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardProtocolSettings {
    pub enabled: bool,
    pub mode: String,
    pub disambiguate_escape_codes: bool,
    pub report_event_types: bool,
    pub report_alternate_keys: bool,
    pub report_all_keys: bool,
}

impl Default for KeyboardProtocolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: "default".to_owned(),
            disambiguate_escape_codes: true,
            report_event_types: true,
            report_alternate_keys: true,
            report_all_keys: false,
        }
    }
}

/// UI mode variants for quick presets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiMode {
    #[default]
    Full,
    Minimal,
    Focused,
}

/// Override for responsive layout detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutModeOverride {
    #[default]
    Auto,
    Compact,
    Standard,
    Wide,
}

/// Reasoning visibility behaviour in the transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningDisplayMode {
    Always,
    #[default]
    Toggle,
    Hidden,
}

/// Default collapse state of agent thinking/reasoning blocks in the transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThinkingBlockState {
    /// Thinking blocks render collapsed (a single summary line) by default.
    #[default]
    Collapsed,
    /// Thinking blocks render fully expanded by default.
    Extended,
}

/// Wizard modal behaviour variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardModalMode {
    /// Traditional multi-step wizard behaviour (Enter advances/collects answers).
    MultiStep,
    /// Tabbed list behaviour (tabs switch categories; Enter submits immediately).
    TabbedList,
}

// ---------------------------------------------------------------------------
// Plan types
// ---------------------------------------------------------------------------

/// A step in an implementation plan.
#[derive(Clone, Debug)]
pub struct PlanStep {
    pub number: usize,
    pub description: String,
    pub details: Option<String>,
    pub files: Vec<String>,
    pub completed: bool,
}

/// A phase in an implementation plan (groups related steps).
#[derive(Clone, Debug)]
pub struct PlanPhase {
    pub name: String,
    pub steps: Vec<PlanStep>,
    pub completed: bool,
}

/// Structured plan content for display in the Implementation Blueprint panel.
#[derive(Clone, Debug)]
pub struct PlanContent {
    pub title: String,
    pub summary: String,
    pub file_path: Option<String>,
    pub phases: Vec<PlanPhase>,
    pub open_questions: Vec<String>,
    pub raw_content: String,
    pub total_steps: usize,
    pub completed_steps: usize,
}

fn finish_phase(phases: &mut Vec<PlanPhase>, current_phase: &mut Option<PlanPhase>) {
    if let Some(phase) = current_phase.take() {
        phases.push(phase);
    }
}

fn add_plan_step(current_phase: &mut Option<PlanPhase>, description: &str, completed: bool) {
    let Some(phase) = current_phase else {
        return;
    };
    phase.steps.push(PlanStep {
        number: phase.steps.len() + 1,
        description: description.to_owned(),
        details: None,
        files: Vec::new(),
        completed,
    });
}

impl PlanContent {
    /// Parse plan content from markdown.
    pub fn from_markdown(title: String, content: &str, file_path: Option<String>) -> Self {
        let mut phases = Vec::new();
        let mut open_questions = Vec::new();
        let mut current_phase: Option<PlanPhase> = None;
        let mut total_steps = 0;
        let mut completed_steps = 0;
        let mut summary = String::new();
        let mut reading_summary = false;

        for line in content.lines() {
            let trimmed = line.trim();

            // The planning prompt emits both conventional markdown headings
            // (`## Summary`) and sparse section labels (`Summary`). Treat
            // either form as a section marker so the label itself is not
            // displayed as the plan summary.
            if trimmed.eq_ignore_ascii_case("summary") || trimmed.eq_ignore_ascii_case("## summary") {
                reading_summary = true;
                continue;
            }

            if reading_summary {
                if trimmed.is_empty() {
                    reading_summary = false;
                    continue;
                }
                if summary.is_empty() {
                    summary = trimmed.to_string();
                }
                reading_summary = false;
                continue;
            }

            // Extract summary from first paragraph
            if summary.is_empty() && !trimmed.is_empty() && !trimmed.starts_with('#') {
                summary = trimmed.to_string();
                continue;
            }

            // Phase headers (## Phase X: ...)
            if let Some(phase_name) = trimmed.strip_prefix("## ") {
                finish_phase(&mut phases, &mut current_phase);
                current_phase = Some(PlanPhase {
                    name: phase_name.to_string(),
                    steps: Vec::new(),
                    completed: false,
                });
                continue;
            }

            // Open questions section
            if trimmed == "## Open Questions" {
                finish_phase(&mut phases, &mut current_phase);
                continue;
            }

            // Step items ([ ] or [x] prefixed)
            if let Some(rest) = trimmed.strip_prefix("[ ] ") {
                total_steps += 1;
                add_plan_step(&mut current_phase, rest, false);
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("[x] ").or_else(|| trimmed.strip_prefix("[X] ")) {
                total_steps += 1;
                completed_steps += 1;
                add_plan_step(&mut current_phase, rest, true);
                continue;
            }

            // Numbered steps (1. **Step 1** ...)
            if trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains('.') {
                total_steps += 1;
                let description = trimmed.split_once('.').map_or("", |(_, rest)| rest).trim();
                add_plan_step(&mut current_phase, description, false);
                continue;
            }

            // Question items
            if trimmed.starts_with("- (") || trimmed.starts_with("- ?") {
                open_questions.push(trimmed.trim_start_matches("- ").to_string());
            }
        }

        // Save last phase
        if let Some(mut phase) = current_phase.take() {
            phase.completed = phase.steps.iter().all(|s| s.completed);
            phases.push(phase);
        }

        // Update phase completion status
        for phase in &mut phases {
            phase.completed = !phase.steps.is_empty() && phase.steps.iter().all(|s| s.completed);
        }

        Self {
            title,
            summary,
            file_path,
            phases,
            open_questions,
            raw_content: content.to_string(),
            total_steps,
            completed_steps,
        }
    }

    /// Get progress as a percentage.
    pub fn progress_percent(&self) -> u8 {
        if self.total_steps == 0 {
            0
        } else {
            let completed_steps = self.completed_steps.to_f32().unwrap_or(f32::MAX);
            let total_steps = self.total_steps.to_f32().unwrap_or(f32::MAX);
            saturating_float_to_u8(completed_steps / total_steps * 100.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PlanContent;

    #[test]
    fn parses_sparse_summary_section_without_displaying_section_label() {
        let plan = PlanContent::from_markdown(
            "Implementation Plan".to_string(),
            "Summary\nFocus on startup latency.\n\n1. Measure startup -> src/startup.rs\n2. Defer refresh -> src/update.rs\n\nValidation\n- cargo check --locked",
            None,
        );

        assert_eq!(plan.summary, "Focus on startup latency.");
        assert_eq!(plan.total_steps, 2);
        assert_eq!(plan.raw_content.lines().next(), Some("Summary"));
    }
}
