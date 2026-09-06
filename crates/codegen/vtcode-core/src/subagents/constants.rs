use crate::config::constants::tools;

pub(crate) const SUBAGENT_TRANSCRIPT_LINE_LIMIT: usize = 200;
pub(crate) const SUBAGENT_MEMORY_BYTES_LIMIT: usize = 25 * 1024;
pub(crate) const SUBAGENT_MEMORY_LINE_LIMIT: usize = 200;
pub(crate) const SUBAGENT_MEMORY_HIGHLIGHT_LIMIT: usize = 4;
pub(crate) const SUBAGENT_MIN_MAX_TURNS: usize = 2;
pub(crate) const SUBAGENT_MIN_BACKGROUND_MAX_TURNS: usize = 4;
pub(crate) const SUBAGENT_PREVIEW_LINES: usize = 24;

pub(crate) const VAGUE_SUBAGENT_PROMPTS: &[&str] = &[
    "analyse",
    "analyse",
    "check",
    "current state",
    "explore",
    "help",
    "inspect",
    "inspect current state",
    "look",
    "look around",
    "report",
    "report findings",
    "report status",
    "review",
    "status",
    "summarise",
    "summarize",
    "summary",
];

pub(crate) const SUBAGENT_TOOL_NAMES: &[&str] = &[
    tools::AGENT,
    tools::SPAWN_AGENT,
    tools::SPAWN_BACKGROUND_SUBPROCESS,
    tools::SEND_INPUT,
    tools::WAIT_AGENT,
    tools::RESUME_AGENT,
    tools::CLOSE_AGENT,
];

/// Subagent-internal tool names that are removed from a child's toolset even
/// when nested delegation is allowed: `spawn_background_subprocess` maps to a
/// dedicated controller guard (`managed_background_runtime`) because it is an
/// argument alias of the unified `agent` registration, not a separate tool.
pub(crate) const CHILD_BLOCKED_BACKGROUND_TOOL_NAMES: &[&str] = &[tools::SPAWN_BACKGROUND_SUBPROCESS];

pub(crate) const NON_MUTATING_TOOL_PREFIXES: &[&str] =
    &[tools::CODE_SEARCH, tools::LIST_SKILLS, tools::LOAD_SKILL_RESOURCE];
