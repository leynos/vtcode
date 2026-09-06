//! System instructions and prompt management.
//!
//! Prompt variants share one canonical base contract plus thin mode deltas and
//! compact runtime addenda. Project-specific behaviour comes from dynamically
//! loaded instruction maps (`AGENTS.md`/`CLAUDE.md`), dynamic tool guidance,
//! skill metadata, and runtime notices.

use crate::config::constants::prompt_budget as prompt_budget_constants;
use crate::config::types::{ShellPromptProfile, SystemPromptMode};
use crate::llm::providers::gemini::wire::Content;
use crate::prompts::context::PromptContext;
use crate::prompts::guidelines::{generate_tool_guidelines_for_profile, render_shell_profile_guidance};
use crate::prompts::output_styles::OutputStyleApplier;
use crate::prompts::render::render_environment_addenda;
use crate::prompts::resources::{apply_system_prompt_layers, resolve_system_prompt_layers};
pub use crate::prompts::static_prompts::{
    agent_identity_label, default_lightweight_prompt, default_system_prompt, lightweight_instruction_text,
    minimal_instruction_text, minimal_system_prompt, specialized_instruction_text, specialized_system_prompt,
    static_profile_prompt,
};
use crate::prompts::system_prompt_cache::PROMPT_CACHE;
use crate::skills::render::render_prompt_skills_section;
use std::path::Path;
use tracing::warn;

/// Shared Planning workflow header used by both static and incremental prompt builders.
pub const PLANNING_WORKFLOW_READ_ONLY_HEADER: &str = "# PLANNING WORKFLOW (READ-ONLY)";
/// Shared Planning workflow notice line describing strict read-only enforcement.
pub const PLANNING_WORKFLOW_READ_ONLY_NOTICE_LINE: &str = "Mutating file edits are blocked, including `apply_patch`. Use `exec_command.cmd` only for read-only repository inspection with the active shell profile's syntax; keep `task_tracker` current.";
/// Shared Planning workflow instruction line for transitioning to implementation.
pub const PLANNING_WORKFLOW_EXIT_INSTRUCTION_LINE: &str = "Only a validated plan persisted under `.vtcode/plans/` is ready for user approval. Mutating tools stay disabled until the user approves.";
/// Canonical contract for model-authored plan output and runtime-owned persistence.
pub const PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE: &str = "Emit exactly one final `<proposed_plan>` block and no surrounding prose. Do not use shell commands or file-writing tools to create or modify `.vtcode/plans/`; runtime owns plan/tracker persistence and validation, and exposes approval controls only after successful persistence.";
/// Compact, spec-like plan quality line. The previous wording ("summary,
/// steps, test cases, assumptions") let the model emit verbosely large plans
/// that blew the generation token budget and were cut off mid-`<proposed_plan>`
/// — which previously re-triggered the recovery loop forever. This mandates a
/// tight spec that fits a small token budget and prefers file:symbol
/// references over prose. It also forbids wrapping those references in
/// markdown link syntax or editor/IDE URI schemes (e.g. `vscode-file://`,
/// `file://`) — plans are read in terminals and other non-hyperlink
/// surfaces, and a bare `path/to/file.rs:42` reference is portable while a
/// broken pseudo-link pointing at the editor binary itself is not.
/// The canonical one-line step format, mirrored from
/// `tools::handlers::planning_workflow::artefacts::CANONICAL_STEP_FORMAT`
/// (const contexts cannot concat!, so the sync is enforced by the
/// `plan_quality_line_shows_canonical_step_format` test below). Showing the
/// exact shape up front matters: the repair directive prints it only after a
/// rejection, and turn_912/913 showed planners repeatedly failing "step lacks
/// a concrete target or verification" without ever seeing an example. The
/// optional `## Expected Outcomes` / `## Dependencies and Prerequisites`
/// sections are requested only "when material" so plans carry outcomes and
/// prerequisites without inflating every plan past the token budget.
pub const PLANNING_WORKFLOW_PLAN_QUALITY_LINE: &str = "Keep the final proposed plan compact and spec-like, with these sections: `## Summary`; `## Implementation Steps` (or `## Steps`); `## Test Cases and Validation` (or `## Validation`); `## Assumptions and Defaults` (or `## Assumptions`). When material to the request, add `## Expected Outcomes` (observable end states the implementation must produce) and `## Dependencies and Prerequisites` (tooling, configuration, or prior work required before implementation); omit them when nothing material exists. Every numbered implementation step must name a concrete file, symbol, behaviour, or other repository target and include one concrete `verify:`/`verification:` command or observable check, written in the canonical one-line form `1. Action -> files: [path/to/file.rs] -> verify: [cargo check]`; generic `1. Do the work` steps, vague prose, and comma-separated verify entries that are not commands or observable checks are not plans. Prefer file:symbol references over prose, written as plain text or inline code (e.g. `src/main.rs:42`) — never as markdown links or editor/IDE URIs (no `[label](url)`, no `vscode-file://`/`file://` schemes). Resolve placeholders and open decisions before approval; use `Next open decision:` or `Open question:` only when a decision remains unresolved.";
/// Scale research effort to the request instead of always exhaustively
/// enumerating the repository. Checkpoint turn_647 showed a "make a simple
/// plan to improve launch time" request burn 70+ tool calls across dozens of
/// files until the turn's tool wall-clock budget was exhausted with no plan
/// delivered — the model had no signal to stop researching and draft. This
/// line gives it a concrete budget to self-regulate against.
pub const PLANNING_WORKFLOW_RESEARCH_SCOPE_LINE: &str = "Scale research to the request: for a narrow or simple ask, ~5-10 targeted reads/searches is usually enough before drafting `<proposed_plan>` — do not exhaustively enumerate the whole repository. For a broad or ambiguous ask, research proportionally more, but stop and draft as soon as scope/decomposition/verification decisions are closed.";
/// Shared Planning workflow policy line directing context-aware read-only research and plain-text question resolution.
pub const PLANNING_WORKFLOW_PLAN_POLICY_LINE: &str = "Continue exploring read-only, finish unblocked planning, and surface open decisions or questions directly in plain text. Monitor the available tool-loop budget; stop research when the plan is sufficiently specified or the limit is near, then synthesize one compact decision-ready plan from the evidence already gathered.";
pub const PLANNING_WORKFLOW_INTERVIEW_POLICY_LINE: &str = "Use repository evidence and reasonable engineering judgment to resolve ordinary ambiguity. Do not ask the user to choose files, implementation details, validation commands, or prioritization you can infer. Use `request_user_input` only for a critical blocker where proceeding could cause materially different, unsafe, or irreversible work; otherwise state the assumption and continue to the plan.";
pub const PLANNING_WORKFLOW_NO_REQUEST_USER_INPUT_POLICY_LINE: &str = "`request_user_input` is optional. If it is unavailable or denied, do not retry it: make reasonable assumptions, synthesize one valid plan from the evidence already gathered, and keep planning active until that plan is persisted and ready for approval.";
/// Shared Planning workflow guard line requiring explicit transition from planning to execution.
pub const PLANNING_WORKFLOW_NO_AUTO_EXIT_LINE: &str = "Do not auto-exit Planning workflow; wait for explicit implementation intent after a validated persisted plan exists.";
/// Shared Planning workflow task-tracking line clarifying availability and aliasing.
/// Implementation prompt used when transitioning from planning to execution.
pub const PLANNING_WORKFLOW_IMPLEMENTATION_PROMPT: &str = "Implement the approved plan. Finish with a concise execution summary covering outcome, changed files, verification performed, and remaining blockers.";
/// Hint shown when planning workflow is active.
pub const PLANNING_WORKFLOW_HINT: &str = "Planning workflow is active. Continue refining; approval controls appear only after a validated plan is persisted.";

pub const PLANNING_WORKFLOW_TASK_TRACKER_LINE: &str = "`task_tracker` remains available while planning.";
/// Shared reminder appended when presenting plans while still in Planning workflow.
pub const PLANNING_WORKFLOW_IMPLEMENT_REMINDER: &str = PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE;

pub const PROMPT_TITLE: &str = "# VT Code";
pub const PROMPT_INTRO: &str = "VT Code. Be concise and safe.";

/// Natural-language role framing inserted between the identity tagline and the
/// contract for the Default and Specialized profiles. Supplies what the terse
/// tagline cannot: the senior-engineer role, the
/// read/plan/implement/verify/report loop, and effort calibration. Omitted
/// from Minimal and Lightweight modes to respect their compact prompt budgets
/// (see the parent-ratio guard in `subagents/config.rs` and the per-mode size
/// guardrails in `tests`). The text intentionally contains no `VT Code`
/// substring so `apply_agent_identity` leaves it untouched when substituting
/// the tagline.
pub const PROMPT_ROLE_PARAGRAPH: &str = "You are a senior engineer in this codebase: read, plan, implement, verify, report. Scale effort; answer simple or non-code questions directly.";
pub const CONTRACT_HEADER: &str = "## Contract";

/// Contract rules shared across all prompt modes that are not universal
/// user-facing runtime guidance.
pub const SHARED_CONTRACT_LINES: &[&str] = &[
    "Preserve task goal, tracker state, touched files, verification status, and decisions across compaction.",
    "`spool_path` holds full tool output. Inspect it once with a targeted shell command through `exec_command.cmd` instead of repeatedly dumping the whole file. Past-turn errors are already in history.",
];

/// Default/Lightweight/Specialized mode: expanded contract lines beyond shared rules.
pub const DEFAULT_SPECIFIC_LINES: &[&str] = &[
    "Start with the project instruction map (`AGENTS.md`/`CLAUDE.md`); inspect code first and match local patterns.",
    "Take safe, reversible steps; recover from tool errors with corrected parameters, smaller scope, or one focused clarification.",
    "Ask only for material behaviour, API, UX, or credential changes.",
    "Keep control on the main thread. Delegate bounded, independent work only.",
    "Verify changes yourself; never claim a check passed unless you ran it.",
    "Keep user updates brief and high-signal.",
    "Read files before answering. Never speculate about code you have not opened.",
    "Make only requested changes. When the active agent has tool access, use tools to implement directly; otherwise stay within the active agent mode.",
];

/// Minimal mode has no additional contract lines; universal behaviour lives in
/// the compiled runtime-guidance section shared by every profile.
pub const MINIMAL_SPECIFIC_LINES: &[&str] = &[];

pub const DEFAULT_OPERATING_PROFILE_DELTA: &str = r#"## Operating Profile

- Core tools are `exec_command`, `write_stdin`, and `apply_patch`; `code_search` unlocks during Planning workflow.
- Put normal shell commands in `exec_command.cmd`; they are not separate function tools. Follow the active shell profile's syntax.
- Treat completion language as a checkpoint, not proof; only stop when verification is resolved.
- When tools are available, read and search before answering; implement directly rather than describing what should be done.
- Use Planning workflow for research/spec work; stay read-only until implementation intent is explicit.
- For demanding, ambiguous, or multi-phase tasks, suggest `start_planning` and wait for user confirmation before entering it."#;

pub const MINIMAL_OPERATING_PROFILE_DELTA: &str = r#"## Operating Profile

- Stay precise; use `task_tracker` once work stops being trivial.
- Treat completion language as a checkpoint.
- Use the project instruction map (`AGENTS.md` and `CLAUDE.md`); open repo docs only when structural rules matter."#;

pub const LIGHTWEIGHT_OPERATING_PROFILE_DELTA: &str = r#"## Operating Profile

- Act and verify in one thread.
- Completion language is a checkpoint.
- Use `task_tracker` for nontrivial work.
- Suggest `start_planning` for demanding or ambiguous multi-phase tasks; the user must confirm entry."#;

pub const SPECIALIZED_OPERATING_PROFILE_DELTA: &str = r#"## Operating Profile

- Explore, plan, then execute.
- Use `task_tracker` for multi-step work and Planning workflow when scope or verification is still open.
- Treat completion language as a checkpoint, not proof; only stop when tracker state, verification, and resumable state agree.
- End plan work with one `<proposed_plan>` block; if a path stalls, re-plan into smaller verified slices.
- Use the project instruction map (`AGENTS.md` and `CLAUDE.md`) plus `docs/harness/ARCHITECTURAL_INVARIANTS.md` when repo-wide invariants matter."#;

const STRUCTURED_REASONING_INSTRUCTIONS: &str = r#"
## Structured Reasoning

Use tags when helpful: `<analysis>` facts/options, `<reasoning_plan>` advisory steps, `<uncertainty>` blockers, `<verification>` checks. Reserve `<plan>` for the planning workflow's approval artefact. When a decision must be consumed by code or tools, prefer JSON or function-call shaped output over prose.
"#;

/// System instruction configuration
#[derive(Debug, Clone, Default)]
pub struct SystemPromptConfig;

/// A named layer of the composed system prompt.
///
/// The token-budget trimmer (see [`SectionKind::trim_priority`]) drops whole
/// sections rather than truncating text mid-layer, so each section's text is
/// stored verbatim (including any leading/trailing whitespace baked into its
/// source constant) exactly as it would have been appended by the legacy
/// single-string builder.
struct PromptSection {
    kind: SectionKind,
    text: String,
}

/// Identifies which layer of the system prompt a `PromptSection` belongs to.
///
/// Variants mirror the layers `compose_system_instruction_text` actually
/// assembles today. Agent identity is not a separate variant: it is applied
/// as an in-place text substitution on the base contract (title/intro lines)
/// rather than an appended section, so it is folded into [`Self::BaseContract`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    /// Compiled runtime guidance, canonical contract + operating profile (with
    /// any workspace prompt-layer override/append and agent-identity
    /// substitution already applied).
    /// Always present and never trimmed to satisfy the token budget.
    BaseContract,
    /// Optional `<analysis>/<reasoning_plan>/<uncertainty>/<verification>` tagging
    /// guidance. Advisory; trimmed first when over budget.
    StructuredReasoning,
    /// Lean "## Skills" routing section rendered from available skill
    /// metadata. Advisory; trimmed alongside structured reasoning.
    Skills,
    /// "## Environment" addenda (languages, interaction mode, MCP sources,
    /// temporal context, working directory).
    EnvironmentAddenda,
    /// "## Active Tools" dynamic tool guidance derived from the active tool
    /// catalogue.
    ToolGuidelines,
    /// "## Shell Profile" guidance for the current command environment.
    ShellProfile,
}

impl SectionKind {
    /// Static section name used in [`SystemPromptReport::trimmed_sections`].
    const fn name(self) -> &'static str {
        match self {
            Self::BaseContract => "base_contract",
            Self::StructuredReasoning => "structured_reasoning",
            Self::Skills => "skills",
            Self::EnvironmentAddenda => "environment_addenda",
            Self::ToolGuidelines => "tool_guidelines",
            Self::ShellProfile => "shell_profile",
        }
    }

    /// Trim order: lower values are dropped first. `None` means the section
    /// is never dropped to satisfy the token budget.
    const fn trim_priority(self) -> Option<u8> {
        match self {
            Self::StructuredReasoning => Some(0),
            Self::Skills => Some(1),
            Self::EnvironmentAddenda => Some(2),
            Self::ShellProfile => Some(3),
            Self::ToolGuidelines => Some(4),
            Self::BaseContract => None,
        }
    }
}

/// Result of measuring a composed system prompt against the configured token
/// budget (`agent.max_system_prompt_tokens`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemPromptReport {
    /// `estimate_token_count` of the final composed text (after trimming, if
    /// trimming occurred).
    pub token_estimate: u64,
    /// Whether `token_estimate` exceeds `agent.max_system_prompt_tokens`.
    pub over_budget: bool,
    /// Names of sections dropped to satisfy the budget, in drop order. Empty
    /// unless `agent.trim_system_prompt` is enabled and trimming occurred.
    pub trimmed_sections: Vec<&'static str>,
}

impl SystemPromptReport {
    /// Measure `text` against `max_tokens` with no trimming applied.
    ///
    /// Useful when a system prompt was assembled or overridden outside the
    /// normal section-based pipeline (e.g. downstream embedders calling
    /// `AgentRunner::set_system_prompt`, or appendix text appended after
    /// [`compose_system_instruction_with_report`] already measured the
    /// sectioned prompt).
    #[must_use]
    pub fn measure(text: &str, max_tokens: u64) -> Self {
        let token_estimate = estimate_token_count(text);
        Self {
            token_estimate,
            over_budget: token_estimate > max_tokens,
            trimmed_sections: Vec::new(),
        }
    }
}

/// Compose the base system instruction plus compact tool/skill/environment addenda.
pub async fn compose_system_instruction_text(
    project_root: &Path,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
    prompt_context: Option<&PromptContext>,
) -> String {
    compose_system_instruction_with_report(project_root, vtcode_config, prompt_context)
        .await
        .0
}

/// Compose the system instruction and return the token-budget report
/// alongside it. See [`SystemPromptReport`] and `SectionKind::trim_priority`
/// for the budget/trim behaviour driven by `agent.max_system_prompt_tokens`,
/// `agent.system_prompt_budget_warning`, and `agent.trim_system_prompt`.
pub async fn compose_system_instruction_with_report(
    project_root: &Path,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
    prompt_context: Option<&PromptContext>,
) -> (String, SystemPromptReport) {
    let sections = build_prompt_sections(project_root, vtcode_config, prompt_context).await;
    let (max_tokens, warn_enabled, trim_enabled) = system_prompt_budget_settings(vtcode_config);
    apply_token_budget(sections, max_tokens, warn_enabled, trim_enabled)
}

/// Measure the system prompt size without applying budget trimming or warnings.
///
/// This is used at startup to warn about potential token budget overruns
/// before the first request is made. Unlike [`compose_system_instruction_with_report`],
/// this function does not apply `agent.trim_system_prompt` and does not emit
/// budget-exceeded warnings.
pub async fn measure_system_prompt_size(
    project_root: &Path,
    vtcode_config: &crate::config::VTCodeConfig,
) -> SystemPromptReport {
    let sections = build_prompt_sections(project_root, Some(vtcode_config), None).await;
    let text = join_prompt_sections(&sections);
    let token_estimate = estimate_token_count(&text);
    SystemPromptReport {
        token_estimate,
        over_budget: token_estimate > vtcode_config.agent.max_system_prompt_tokens,
        trimmed_sections: Vec::new(),
    }
}

/// Resolve the effective `(max_system_prompt_tokens, budget_warning_enabled,
/// trim_enabled)` settings, falling back to the `AgentConfig` defaults when
/// no config is available.
fn system_prompt_budget_settings(vtcode_config: Option<&crate::config::VTCodeConfig>) -> (u64, bool, bool) {
    vtcode_config.map_or((prompt_budget_constants::DEFAULT_MAX_SYSTEM_PROMPT_TOKENS, true, false), |cfg| {
        (
            cfg.agent.max_system_prompt_tokens,
            cfg.agent.system_prompt_budget_warning,
            cfg.agent.trim_system_prompt,
        )
    })
}

/// Build the ordered prompt sections. Each section's text is stored exactly
/// as the legacy single-string builder would have appended it, so
/// [`join_prompt_sections`] reproduces byte-identical output when nothing is
/// trimmed.
async fn build_prompt_sections(
    project_root: &Path,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
    prompt_context: Option<&PromptContext>,
) -> Vec<PromptSection> {
    let prompt_mode = vtcode_config
        .map(|c| c.agent.system_prompt_mode)
        .unwrap_or(SystemPromptMode::Default);
    let static_base_prompt = static_profile_prompt(prompt_mode);
    let resolved_layers = resolve_system_prompt_layers(project_root).await;
    let mut base_prompt = apply_system_prompt_layers(static_base_prompt, &resolved_layers);
    crate::prompts::runtime_guidance::ensure_runtime_guidance(&mut base_prompt);

    tracing::trace!(
        mode = ?prompt_mode,
        base_tokens_approx = base_prompt.len() / 4, // rough token estimate
        "Selected system prompt mode"
    );

    // Apply agent identity based on the default primary agent configuration.
    // This combines "VT Code" with the active agent mode so the LLM knows its role.
    if let Some(cfg) = vtcode_config {
        let agent_label = agent_identity_label(&cfg.default_primary_agent);
        base_prompt = apply_agent_identity(&base_prompt, &agent_label);
    }

    let mut sections = vec![PromptSection { kind: SectionKind::BaseContract, text: base_prompt }];

    if should_include_structured_reasoning(vtcode_config, prompt_mode) {
        sections.push(PromptSection {
            kind: SectionKind::StructuredReasoning,
            text: STRUCTURED_REASONING_INSTRUCTIONS.to_string(),
        });
    }

    let shell_profile = vtcode_config
        .map(|cfg| cfg.agent.shell_prompt_profile)
        .unwrap_or(ShellPromptProfile::Auto)
        .resolve_for_current_platform();
    sections.push(PromptSection {
        kind: SectionKind::ShellProfile,
        text: render_shell_profile_guidance(shell_profile),
    });

    if let Some(ctx) = prompt_context {
        let guidelines =
            generate_tool_guidelines_for_profile(&ctx.available_tools, ctx.capability_level, shell_profile);
        if !guidelines.is_empty() {
            sections.push(PromptSection {
                kind: SectionKind::ToolGuidelines,
                text: guidelines.trim_start_matches('\n').to_string(),
            });
        }
        if let Some(skills_section) = render_prompt_skills_section(&ctx.available_skill_metadata) {
            sections.push(PromptSection { kind: SectionKind::Skills, text: skills_section });
        }
    }

    if let Some(environment_section) = render_environment_addenda(vtcode_config, prompt_context) {
        sections.push(PromptSection {
            kind: SectionKind::EnvironmentAddenda,
            text: environment_section,
        });
    }

    sections
}

/// Join ordered prompt sections exactly as the legacy single-string builder
/// did: the first section verbatim, then each subsequent section separated
/// by a blank line.
fn join_prompt_sections(sections: &[PromptSection]) -> String {
    let capacity = sections.iter().map(|section| section.text.len() + 2).sum();
    let mut joined = String::with_capacity(capacity);
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            joined.push_str("\n\n");
        }
        joined.push_str(&section.text);
    }
    joined
}

/// Enforce the configured system-prompt token budget against the composed
/// sections.
///
/// When under budget, sections are joined and returned unchanged. When over
/// budget and `trim_enabled` is false, the full untrimmed text is still used
/// but a warning is logged (gated on `warn_enabled`). When over budget and
/// `trim_enabled` is true, whole sections are dropped in
/// [`SectionKind::trim_priority`] order (lowest first), re-measuring after
/// each drop, until the prompt fits or only untrimmable sections remain.
fn apply_token_budget(
    mut sections: Vec<PromptSection>,
    max_tokens: u64,
    warn_enabled: bool,
    trim_enabled: bool,
) -> (String, SystemPromptReport) {
    let mut text = join_prompt_sections(&sections);
    let mut token_estimate = estimate_token_count(&text);
    let mut trimmed_sections: Vec<&'static str> = Vec::new();

    if token_estimate > max_tokens {
        if trim_enabled {
            while token_estimate > max_tokens {
                let drop_index = sections
                    .iter()
                    .enumerate()
                    .filter_map(|(index, section)| section.kind.trim_priority().map(|priority| (priority, index)))
                    .min_by_key(|(priority, _)| *priority)
                    .map(|(_, index)| index);
                let Some(drop_index) = drop_index else {
                    break;
                };
                let dropped = sections.remove(drop_index);
                trimmed_sections.push(dropped.kind.name());
                text = join_prompt_sections(&sections);
                token_estimate = estimate_token_count(&text);
            }

            if !trimmed_sections.is_empty() {
                tracing::warn!(
                    token_estimate,
                    max_system_prompt_tokens = max_tokens,
                    dropped_sections = ?trimmed_sections,
                    "Trimmed system prompt sections to satisfy token budget"
                );
            }
        } else if warn_enabled {
            tracing::warn!(
                token_estimate,
                max_system_prompt_tokens = max_tokens,
                "System prompt exceeds configured token budget"
            );
        }
    }

    let report = SystemPromptReport {
        token_estimate,
        over_budget: token_estimate > max_tokens,
        trimmed_sections,
    };
    (text, report)
}

/// Apply agent identity to the system prompt by replacing the title and intro lines.
/// This combines the "VT Code" identity with the active agent mode so the LLM
/// knows its role (e.g., "VT Code (Build mode)" or "VT Code (Auto mode)").
fn apply_agent_identity(prompt: &str, agent_label: &str) -> String {
    let mut result = prompt.to_string();
    let old_title = PROMPT_TITLE;
    let old_intro = PROMPT_INTRO;

    let title_found = if let Some(pos) = result.find(old_title) {
        result.replace_range(pos..pos + old_title.len(), &format!("# {agent_label}"));
        true
    } else {
        warn!("Could not find prompt title '{}' to apply agent identity", old_title);
        false
    };

    let intro_found = if let Some(pos) = result.find(old_intro) {
        result.replace_range(pos..pos + old_intro.len(), &format!("{agent_label}. Be concise and safe."));
        true
    } else {
        warn!("Could not find prompt intro '{}' to apply agent identity", old_intro);
        false
    };

    if !title_found || !intro_found {
        warn!(
            agent_label = %agent_label,
            title_replaced = title_found,
            intro_replaced = intro_found,
            "agent identity partially applied"
        );
    }

    result
}

fn should_include_structured_reasoning(
    vtcode_config: Option<&crate::config::VTCodeConfig>,
    mode: SystemPromptMode,
) -> bool {
    if let Some(cfg) = vtcode_config {
        return cfg.agent.should_include_structured_reasoning_tags();
    }

    // Backward-compatible fallback when no config is available.
    matches!(mode, SystemPromptMode::Specialized)
}

/// Generate the stable base system instruction with configuration-aware sections.
///
/// Note: This function maintains backward compatibility by not accepting prompt_context.
/// For enhanced prompts with dynamic guidelines, call `compose_system_instruction_text` directly.
pub async fn generate_system_instruction_with_config(
    config: &SystemPromptConfig,
    project_root: &Path,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
) -> Content {
    let (content, _report) =
        generate_system_instruction_with_config_and_report(config, project_root, vtcode_config).await;
    content
}

/// Same as [`generate_system_instruction_with_config`] but also returns the
/// [`SystemPromptReport`] for the composed prompt, whether served from cache
/// or freshly built.
pub async fn generate_system_instruction_with_config_and_report(
    _config: &SystemPromptConfig,
    project_root: &Path,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
) -> (Content, SystemPromptReport) {
    let cache_key = cache_key(project_root, vtcode_config, None);
    let (instruction, report) = match PROMPT_CACHE.get(&cache_key) {
        Some(cached) => cached,
        None => {
            let built = compose_system_instruction_with_report(project_root, vtcode_config, None).await;
            PROMPT_CACHE.insert(cache_key, built.clone());
            built
        }
    };

    // Apply output style if configured
    let styled_instruction = apply_output_style(instruction, vtcode_config, project_root).await;
    (Content::system_text(styled_instruction), report)
}

/// Apply output style to a generated system instruction
pub async fn apply_output_style(
    instruction: String,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
    project_root: &Path,
) -> String {
    if let Some(config) = vtcode_config {
        let output_style_applier = OutputStyleApplier::new();
        if let Err(e) = output_style_applier.load_styles_from_config(config, project_root).await {
            tracing::warn!("Failed to load output styles: {}", e);
            instruction // Return original if loading fails
        } else {
            output_style_applier
                .apply_style(&config.output_style.active_style, &instruction, config)
                .await
        }
    } else {
        instruction // Return original if no config
    }
}

/// Build a cache key for the system prompt.
///
/// `catalogue_epoch` is the tool-catalogue version at the time of the request. When
/// the tool set changes (e.g. planning workflow is toggled, MCP tools are refreshed), the
/// epoch advances and the old cached prompt is superseded rather than served stale.
/// Pass `None` to get the same behaviour as before epoch tracking was introduced.
fn cache_key(
    project_root: &Path,
    vtcode_config: Option<&crate::config::VTCodeConfig>,
    catalogue_epoch: Option<u64>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    project_root.hash(&mut hasher);

    if let Some(cfg) = vtcode_config {
        cfg.agent.include_working_directory.hash(&mut hasher);
        cfg.agent.include_temporal_context.hash(&mut hasher);
        cfg.prompt_cache.cache_friendly_prompt_shaping.hash(&mut hasher);
        cfg.agent.include_structured_reasoning_tags.hash(&mut hasher);
        std::mem::discriminant(&cfg.agent.system_prompt_mode).hash(&mut hasher);
        std::mem::discriminant(&cfg.agent.tool_documentation_mode).hash(&mut hasher);
        cfg.agent.max_system_prompt_tokens.hash(&mut hasher);
        cfg.agent.system_prompt_budget_warning.hash(&mut hasher);
        cfg.agent.trim_system_prompt.hash(&mut hasher);
        cfg.default_primary_agent.hash(&mut hasher);
    } else {
        "default".hash(&mut hasher);
    }

    catalogue_epoch.unwrap_or(0).hash(&mut hasher);

    format!("sys_prompt:{:016x}", hasher.finish())
}

/// Generate a minimal system instruction (pi-inspired, <1K tokens)
pub fn generate_minimal_instruction() -> Content {
    Content::system_text(minimal_instruction_text())
}

/// Generate a lightweight system instruction for simple operations
pub fn generate_lightweight_instruction() -> Content {
    Content::system_text(lightweight_instruction_text())
}

/// Generate a specialized system instruction for advanced operations
pub fn generate_specialized_instruction() -> Content {
    Content::system_text(specialized_instruction_text())
}

// ─── Token Estimation ────────────────────────────────────────────────────────

/// Fast character-based token count estimation.
///
/// Uses the heuristic `tokens ~= chars / 4` which is accurate within ~20%
/// for English text with code. This is intentionally approximate — the goal
/// is monitoring and budget enforcement, not precise accounting.
#[must_use]
pub fn estimate_token_count(text: &str) -> u64 {
    // Round up to avoid underestimation
    text.len().div_ceil(4) as u64
}

/// Tests for system-prompt modes, composition, dynamic resources, and budgets.
#[cfg(test)]
#[path = "system_tests/mod.rs"]
mod tests;
