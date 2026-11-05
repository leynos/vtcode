#![expect(
    clippy::cast_possible_truncation,
    reason = "Progress percentages are clamped to the documented byte-sized display range."
)]

//! Pure data types with no dependencies beyond `std`.

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

mod plan;

pub use plan::{PlanContent, PlanPhase, PlanStep};
