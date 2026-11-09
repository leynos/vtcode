//! Pure data types with no dependencies beyond `std`.

/// Message kind tag for inline transcript lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineMessageKind {
    /// Output produced by the agent.
    Agent,
    /// An error report.
    Error,
    /// Informational notice.
    Info,
    /// Policy or permission notice.
    Policy,
    /// Raw PTY output.
    Pty,
    /// Tool invocation output.
    Tool,
    /// User input.
    User,
    /// A warning.
    Warning,
}

/// A single slash-command entry for the suggestion palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandItem {
    /// Command name, including the leading slash.
    pub name: String,
    /// One-line description shown in the palette.
    pub description: String,
}

impl SlashCommandItem {
    /// Create a command entry from its name and description.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }
}

/// Search configuration for a list overlay.
#[derive(Clone, Debug)]
pub struct InlineListSearchConfig {
    /// Label displayed above the search field.
    pub label: String,
    /// Placeholder shown when the search field is empty.
    pub placeholder: Option<String>,
}

/// Configuration for a secure (masked) prompt input.
#[derive(Clone, Debug)]
pub struct SecurePromptConfig {
    /// Label displayed above the input.
    pub label: String,
    /// Optional placeholder shown when input is empty.
    pub placeholder: Option<String>,
    /// Whether the input should be masked (e.g., API keys).
    pub mask_input: bool,
}

/// Standalone surface preference for selecting inline vs alternate rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSurface {
    /// Choose the surface automatically for the terminal.
    Auto,
    /// Render on the alternate screen.
    Alternate,
    #[default]
    /// Render inline in the scrollback.
    Inline,
}

/// Standalone keyboard protocol settings for terminal key event enhancements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardProtocolSettings {
    /// Whether enhanced keyboard reporting is enabled.
    pub enabled: bool,
    /// Keyboard protocol mode name.
    pub mode: String,
    /// Whether escape sequences are disambiguated.
    pub disambiguate_escape_codes: bool,
    /// Whether key event types are reported.
    pub report_event_types: bool,
    /// Whether alternate keys are reported.
    pub report_alternate_keys: bool,
    /// Whether all keys are reported as escape sequences.
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
    /// Full layout with every panel.
    Full,
    /// Minimal layout for narrow terminals.
    Minimal,
    /// Focused layout emphasizing the transcript.
    Focused,
}

/// Override for responsive layout detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutModeOverride {
    #[default]
    /// Let responsive layout detection choose.
    Auto,
    /// Force the compact layout.
    Compact,
    /// Force the standard layout.
    Standard,
    /// Force the wide layout.
    Wide,
}

/// Reasoning visibility behaviour in the transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningDisplayMode {
    /// Always show reasoning.
    Always,
    #[default]
    /// Show reasoning only when toggled on.
    Toggle,
    /// Never show reasoning.
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

mod plan;

pub use plan::{PlanContent, PlanPhase, PlanStep};
