//! Implementation-plan data extracted from the original UI protocol types module.

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
                if !trimmed.is_empty() {
                    if summary.is_empty() {
                        summary = trimmed.to_string();
                    }
                    reading_summary = false;
                }
                continue;
            }

            // Extract summary from first paragraph
            if summary.is_empty() && !trimmed.is_empty() && !trimmed.starts_with('#') {
                summary = trimmed.to_string();
                continue;
            }

            // Phase headers (## Phase X: ...)
            if let Some(phase_name) = trimmed.strip_prefix("## ") {
                if let Some(phase) = current_phase.take() {
                    phases.push(phase);
                }
                current_phase = Some(PlanPhase {
                    name: phase_name.to_string(),
                    steps: Vec::new(),
                    completed: false,
                });
                continue;
            }

            // Open questions section
            if trimmed == "## Open Questions" {
                if let Some(phase) = current_phase.take() {
                    phases.push(phase);
                }
                continue;
            }

            // Step items ([ ] or [x] prefixed)
            if let Some(rest) = trimmed.strip_prefix("[ ] ") {
                total_steps += 1;
                if let Some(ref mut phase) = current_phase {
                    phase.steps.push(PlanStep {
                        number: phase.steps.len() + 1,
                        description: rest.to_string(),
                        details: None,
                        files: Vec::new(),
                        completed: false,
                    });
                }
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("[x] ").or_else(|| trimmed.strip_prefix("[X] ")) {
                total_steps += 1;
                completed_steps += 1;
                if let Some(ref mut phase) = current_phase {
                    phase.steps.push(PlanStep {
                        number: phase.steps.len() + 1,
                        description: rest.to_string(),
                        details: None,
                        files: Vec::new(),
                        completed: true,
                    });
                }
                continue;
            }

            // Numbered steps (1. **Step 1** ...)
            if trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains('.') {
                total_steps += 1;
                if let Some(ref mut phase) = current_phase {
                    let desc = trimmed.split_once('.').map(|x| x.1).unwrap_or("").trim();
                    phase.steps.push(PlanStep {
                        number: phase.steps.len() + 1,
                        description: desc.to_string(),
                        details: None,
                        files: Vec::new(),
                        completed: false,
                    });
                }
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
    #[allow(
        clippy::cast_sign_loss,
        reason = "Intentional compatibility, platform, or test-only suppression."
    )]
    pub fn progress_percent(&self) -> u8 {
        if self.total_steps == 0 {
            0
        } else {
            ((self.completed_steps as f32 / self.total_steps as f32) * 100.0) as u8
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
