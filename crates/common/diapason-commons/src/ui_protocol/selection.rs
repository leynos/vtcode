//! List selection and wizard step types.

/// Rewind action choices for the rewind overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindAction {
    /// Restore both code and conversation to the checkpoint.
    RestoreBoth,
    /// Restore only the conversation.
    RestoreConversation,
    /// Restore only the code changes.
    RestoreCode,
    /// Summarize the conversation from this point onward.
    SummarizeFromHere,
    /// Leave the checkpoint untouched.
    NeverMind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Processing tier requested from the OpenAI service.
pub enum OpenAIServiceTierChoice {
    /// Use the project's configured default tier.
    ProjectDefault,
    /// Use the discounted flex tier.
    Flex,
    /// Use the higher-priority tier.
    Priority,
}

/// Selection value returned from a list or wizard overlay.
///
/// The `Reasoning` variant carries a `String` reasoning-effort level rather
/// than a typed enum so that this type stays free of config-crate dependencies.
/// Callers convert to/from their local `ReasoningEffortLevel` as needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineListSelection {
    /// Select the model at this index in the available-model list.
    Model(usize),
    /// Select the dynamically discovered model at this index.
    DynamicModel(usize),
    /// Select the custom provider at this index.
    CustomProvider(usize),
    /// Reload the dynamically discovered model list.
    RefreshDynamicModels,
    /// Set the reasoning-effort level to the given identifier.
    Reasoning(String),
    /// Turn configurable reasoning off.
    DisableReasoning,
    /// Select an OpenAI service tier.
    OpenAIServiceTier(OpenAIServiceTierChoice),
    /// Enter a custom model identifier.
    CustomModel,
    /// Select the theme with the given name.
    Theme(String),
    /// Select the session with the given identifier.
    Session(String),
    /// Begin a new session forked from an existing one.
    SessionForkMode {
        /// Identifier of the session to fork.
        session_id: String,
        /// Whether to summarize the forked history.
        summarize: bool,
    },
    /// Run the configuration action with the given identifier.
    ConfigAction(String),
    /// Run the slash command with the given name.
    SlashCommand(String),
    /// Record a standing tool-approval decision.
    ToolApproval(bool),
    /// Deny this tool call once.
    ToolApprovalDenyOnce,
    /// Allow this tool for the rest of the session.
    ToolApprovalSession,
    /// Allow this tool permanently.
    ToolApprovalPermanent,
    /// Enable the tool so future calls auto-approve.
    ToolApprovalEnable,
    /// Reload the file from disk, discarding local edits.
    FileConflictReload,
    /// View the diff between conflicting versions.
    FileConflictViewDiff,
    /// Abort the conflicting operation.
    FileConflictAbort,
    /// Raise the session limit to the given value.
    SessionLimitIncrease(usize),
    /// Rewind to the checkpoint at this index.
    RewindCheckpoint(usize),
    /// Apply the chosen rewind action.
    RewindAction(RewindAction),

    /// Selection shape used by legacy tabbed HITL flows.
    AskUserChoice {
        /// Identifier of the tab that produced the answer.
        tab_id: String,
        /// Identifier of the chosen item.
        choice_id: String,
        /// Free-form text supplied alongside the choice.
        text: Option<String>,
    },

    /// Selection returned from the `request_user_input` HITL tool.
    RequestUserInputAnswer {
        /// Identifier of the answered question.
        question_id: String,
        /// Selected option identifiers.
        selected: Vec<String>,
        /// Free-form answer supplied instead of a listed option.
        other: Option<String>,
    },

    /// Plan confirmation dialog result (human-in-the-loop flow).
    PlanApprovalExecute,
    /// Execute the approved plan after clearing transient context.
    PlanApprovalFreshContext,
    /// Return to planning to edit the plan file.
    PlanApprovalEditPlan,
    /// Return to planning to discuss and revise the plan in chat.
    PlanApprovalDiscuss,
    /// Auto-accept all future plans in this session.
    PlanApprovalAutoAccept,
    /// Hand off to the build primary agent and execute the plan.
    PlanApprovalSwitchBuild,
    /// Hand off to the auto primary agent (auto-execute with per-step HITL).
    PlanApprovalSwitchAuto,
}

/// A selectable item inside a list overlay.
#[derive(Clone, Debug)]
pub struct InlineListItem {
    /// Primary item label.
    pub title: String,
    /// Secondary label shown beneath the title.
    pub subtitle: Option<String>,
    /// Short badge displayed beside the title.
    pub badge: Option<String>,
    /// Indentation level applied when rendering.
    pub indent: u8,
    /// Selection value emitted when this item is chosen.
    pub selection: Option<InlineListSelection>,
    /// Text matched by list search.
    pub search_value: Option<String>,
}

/// A single step in a wizard modal flow.
#[derive(Clone, Debug)]
pub struct WizardStep {
    /// Title displayed in the tab header.
    pub title: String,
    /// Question or instruction shown above the list.
    pub question: String,
    /// Selectable items for this step.
    pub items: Vec<InlineListItem>,
    /// Whether this step has been completed.
    pub completed: bool,
    /// The selected answer for this step (if completed).
    pub answer: Option<InlineListSelection>,

    /// Whether the step accepts free-form input.
    pub allow_freeform: bool,
    /// Label for the free-form input.
    pub freeform_label: Option<String>,
    /// Placeholder shown when the free-form input is empty.
    pub freeform_placeholder: Option<String>,
    /// Default value pre-filled in the free-form input.
    pub freeform_default: Option<String>,
}
