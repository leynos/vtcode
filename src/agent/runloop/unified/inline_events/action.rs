use crate::agent::runloop::unified::planning_workflow::PlanExecutionContext;
use diapason_core::hooks::SessionEndReason;
use diapason_ui::tui::app::SubmittedInput;

pub(crate) enum InlineLoopAction {
    Continue,
    Submit(SubmittedInput),
    SubmitPrompt(SubmittedInput),
    SubmitQueued(super::queue::QueuedInput),
    CyclePrimaryAgent,
    CyclePrimaryAgentPrevious,
    SelectPrimaryAgent {
        name: Option<String>,
    },
    RequestInlinePromptSuggestion(String),
    OpenToolOutputInEditor(String),
    OpenToolOutputScrollback(String),
    Exit(SessionEndReason),
    ResumeSession(String), // Session identifier to resume
    ForkSession {
        session_id: String,
        summarize: bool,
    },
    /// Plan approved (Claude Code style HITL) - continue with implementation
    PlanApproved {
        execution_context: PlanExecutionContext,
    },
    /// User wants to return to planning workflow to edit the plan
    PlanEditRequested,
    /// Diff preview approved - apply the edit changes
    DiffApproved,
    /// Diff preview rejected - cancel the edit changes
    DiffRejected,
    /// Launch external editor pre-populated with the given draft text
    LaunchEditorWithDraft {
        draft: String,
    },
}
