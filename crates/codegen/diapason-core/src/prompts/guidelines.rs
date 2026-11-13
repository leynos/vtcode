//! Tool guidance rendered into the system prompt.
//!
//! Generates compact usage guidance for the tool set exposed in a session,
//! including shell profile and capability-specific hints.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::config::constants::tools;
use crate::config::types::{CapabilityLevel, ResolvedShellPromptProfile, ShellPromptProfile};
use crate::core::agent::harness_kernel::SessionToolCatalogueSnapshot;
use crate::llm::provider::ToolDefinition;
use crate::prompts::sections::SectionBoundaryMode;
use crate::tools::registry::tool_groups;

const TOOL_EXEC_COMMAND: &str = tools::EXEC_COMMAND;
const TOOL_WRITE_STDIN: &str = tools::WRITE_STDIN;
const TOOL_CODE_SEARCH: &str = tools::CODE_SEARCH;
const TOOL_READ_FILE: &str = tools::READ_FILE;
const TOOL_LIST_FILES: &str = tools::LIST_FILES;
const TOOL_APPLY_PATCH: &str = tools::APPLY_PATCH;
const TOOL_REQUEST_USER_INPUT: &str = tools::REQUEST_USER_INPUT;
const TOOL_TASK_TRACKER: &str = tools::TASK_TRACKER;
const TOOL_START_PLANNING: &str = tools::START_PLANNING;

/// Generate compact cross-tool guidance based on the tools available in the session.
pub fn generate_tool_guidelines(available_tools: &[String], capability_level: Option<CapabilityLevel>) -> String {
    generate_tool_guidelines_for_profile(
        available_tools,
        capability_level,
        ShellPromptProfile::Auto.resolve_for_current_platform(),
    )
}

/// Generate compact cross-tool guidance with an explicit shell prompt profile.
pub fn generate_tool_guidelines_for_profile(
    available_tools: &[String],
    capability_level: Option<CapabilityLevel>,
    shell_profile: ResolvedShellPromptProfile,
) -> String {
    let has_exec = available_tools.iter().any(|tool| tool == TOOL_EXEC_COMMAND);
    let has_stdin = available_tools.iter().any(|tool| tool == TOOL_WRITE_STDIN);
    let has_search = available_tools.iter().any(|tool| tool == TOOL_CODE_SEARCH);
    let has_read_file = available_tools.iter().any(|tool| tool == TOOL_READ_FILE);
    let has_list_files = available_tools.iter().any(|tool| tool == TOOL_LIST_FILES);
    let has_apply_patch = available_tools.iter().any(|tool| tool == TOOL_APPLY_PATCH);
    let has_start_planning = available_tools.iter().any(|tool| tool == TOOL_START_PLANNING);

    let mut lines = Vec::new();
    if let Some(mode_line) = capability_mode_line(capability_level, has_exec, has_apply_patch) {
        lines.push(mode_line.to_string());
    }
    if let Some(browse_guidance) =
        browse_tool_guidance(has_exec, has_search, has_list_files, has_read_file, shell_profile)
    {
        lines.push(browse_guidance);
    }
    if has_search || has_read_file || has_list_files {
        lines.push(read_only_batching_guidance(has_read_file).to_string());
    }
    if has_apply_patch {
        lines.push("- Use `apply_patch` after inspection; keep patches small; bound diffs; verify. WebMCP edits are untrusted proposals; terminal permission is authoritative.".to_string());
    }
    if has_exec {
        lines.push(shell_task_guidance(shell_profile).to_string());
    }
    if has_exec || has_apply_patch || has_search || has_read_file || has_list_files {
        lines.push("- Diagnose from evidence; never bypass safeguards. Completion is a checkpoint; keep verification resolved.".to_string());
    }
    if has_stdin {
        lines.push("- Use `write_stdin` only with the existing `session_id` of an active exec_command session; when a run response is still running it includes `next_wait_args` (a pre-filled `action:\"wait\"` call) — prefer it over polling with `next_continue_args` to avoid burning tokens on short polls; call `wait` again after an in-progress deadline, and treat an active `spool_complete: false` reference as readable partial output; an exited pending spool is withheld until a later wait.".to_string());
    }
    if has_search {
        lines.push(code_search_guidance(has_exec, shell_profile).to_string());
    }
    if has_apply_patch || has_exec {
        lines.push("- Repeat calls: re-plan; after `preview_budget_exhausted`, trust preserved outcome metadata; do not repeat or rephrase the call to recover hidden output. If verification remains, run one standalone verifier, then synthesize.".to_string());
    }
    if has_search || has_exec {
        lines.push("- Run in parallel when inputs do not depend; order dependencies.".to_string());
    }
    if has_start_planning {
        lines.push(
            "- For demanding, ambiguous, or multi-phase tasks, call `start_planning` to ask the user before entering the read-only Planning workflow; do not use it for straightforward changes.".to_string(),
        );
    }

    if lines.is_empty() {
        return String::new();
    }

    format!("\n\n## Active Tools\n{}", lines.join("\n"))
}

/// Append refreshed runtime tool guidance using the platform-resolved shell profile.
///
/// Removes any existing `## Active Tools`, `[Runtime Tool Catalogue]` and
/// pre-rename `[Runtime Tool Catalog]` sections before appending the refreshed
/// guidance. The catalogue metadata block is appended only when
/// `include_catalogue_metadata` is true and the snapshot carries tool
/// definitions.
pub fn append_runtime_tool_prompt_sections(
    prompt: &mut String,
    tool_snapshot: &SessionToolCatalogueSnapshot,
    include_catalogue_metadata: bool,
) {
    append_runtime_tool_prompt_sections_for_profile(
        prompt,
        tool_snapshot,
        include_catalogue_metadata,
        ShellPromptProfile::Auto.resolve_for_current_platform(),
    );
}

/// Append refreshed runtime tool guidance for an explicit shell profile.
///
/// Removes any existing `## Active Tools`, `[Runtime Tool Catalogue]` and
/// pre-rename `[Runtime Tool Catalog]` sections before appending the refreshed
/// guidance. The catalogue metadata block is appended only when
/// `include_catalogue_metadata` is true and the snapshot carries tool
/// definitions.
pub fn append_runtime_tool_prompt_sections_for_profile(
    prompt: &mut String,
    tool_snapshot: &SessionToolCatalogueSnapshot,
    include_catalogue_metadata: bool,
    shell_profile: ResolvedShellPromptProfile,
) {
    remove_prompt_section(prompt, "## Active Tools");
    remove_prompt_section(prompt, "[Runtime Tool Catalogue]");
    // Prompts supplied by embedders may still carry the pre-rename section.
    remove_prompt_section(prompt, "[Runtime Tool Catalog]");
    while prompt.ends_with('\n') {
        prompt.pop();
    }

    let available_tools = snapshot_tool_names(tool_snapshot);
    let guidelines =
        generate_runtime_tool_guidelines_for_profile(&available_tools, tool_snapshot.planning_active, shell_profile);
    if !guidelines.is_empty() {
        append_prompt_block(prompt, guidelines.trim_start_matches('\n'));
    }

    if include_catalogue_metadata && tool_snapshot.snapshot.is_some() {
        let active_tools = if tool_snapshot.active_tool_names.is_empty() {
            "none".to_string()
        } else {
            tool_snapshot.active_tool_names.join(", ")
        };
        let catalogue_metadata = format!(
            "[Runtime Tool Catalogue]\n- version: {}\n- epoch: {}\n- catalogue_tools: {}\n- available_tools: {}\n- currently_available_tools: {}\n- request_user_input_enabled: {}",
            tool_snapshot.version,
            tool_snapshot.epoch,
            tool_snapshot.catalogue_tools(),
            tool_snapshot.available_tools(),
            active_tools,
            tool_snapshot.request_user_input_enabled,
        );
        append_prompt_block(prompt, &catalogue_metadata);
    }
}

/// Append a compact summary of tools omitted from a client-local wire payload.
pub fn append_deferred_tools_prompt_section(prompt: &mut String, tools: &[ToolDefinition]) {
    remove_prompt_section(prompt, "[Deferred Tools]");

    let mut lines: Vec<String> = tool_groups(tools)
        .into_iter()
        .filter(|group| group.deferred_count > 0)
        .map(|group| {
            format!("- {} ({} tools): {}", group.name, group.deferred_count, group.description.unwrap_or_default())
        })
        .collect();

    let unnamespaced_deferred = tools
        .iter()
        .filter(|tool| tool.namespace.is_none() && tool.defer_loading == Some(true))
        .count();
    if unnamespaced_deferred > 0 {
        lines.push(format!("- {unnamespaced_deferred} additional deferred tools"));
    }

    if lines.is_empty() {
        return;
    }

    let section = format!(
        "[Deferred Tools]\n{}\nUse `search_tools` to find a deferred capability. Selected definitions become available in the next request segment.",
        lines.join("\n")
    );
    append_prompt_block(prompt, &section);
}

fn append_prompt_block(prompt: &mut String, block: &str) {
    if block.is_empty() {
        return;
    }

    if prompt.is_empty() {
        prompt.push_str(block);
    } else {
        let _ = write!(prompt, "\n\n{block}");
    }
}

fn remove_prompt_section(prompt: &mut String, section_header: &str) {
    while let Some((section_start, section_end)) = find_prompt_section_bounds(prompt, section_header) {
        prompt.replace_range(section_start..section_end, "");
    }
}

/// Locate a runtime prompt section under either its bracket or Markdown form.
fn find_prompt_section_bounds(prompt: &str, section_header: &str) -> Option<(usize, usize)> {
    crate::prompts::sections::find_prompt_section_bounds(prompt, section_header, SectionBoundaryMode::BracketOrMarkdown)
}

/// Render the runtime tool guidance for a shell profile, adding planning-phase
/// browse and edit guidance when planning is active.
fn generate_runtime_tool_guidelines_for_profile(
    available_tools: &[String],
    planning_active: bool,
    shell_profile: ResolvedShellPromptProfile,
) -> String {
    if !planning_active {
        return generate_tool_guidelines_for_profile(available_tools, None, shell_profile);
    }

    let has_exec = available_tools.iter().any(|tool| tool == TOOL_EXEC_COMMAND);
    let has_search = available_tools.iter().any(|tool| tool == TOOL_CODE_SEARCH);
    let has_read_file = available_tools.iter().any(|tool| tool == TOOL_READ_FILE);
    let has_list_files = available_tools.iter().any(|tool| tool == TOOL_LIST_FILES);
    let has_request_user_input = available_tools.iter().any(|tool| tool == TOOL_REQUEST_USER_INPUT);
    let has_task_tracker = available_tools.iter().any(|tool| matches!(tool.as_str(), TOOL_TASK_TRACKER));

    let mut lines = vec!["- Planning workflow active: stay within the read-safe tool list.".to_string()];
    lines.push("- Monitor the available planning tool-loop budget; stop research when the plan is specified or the limit is near, then synthesize one compact decision-ready plan from existing evidence.".to_string());
    lines.push("- Every implementation step in the final plan must name a concrete repository target and include a concrete verification command or observable check.".to_string());
    lines.push("- When the plan is ready, emit only one `<proposed_plan>` block; do not repeat planning policy text or add surrounding prose.".to_string());
    if let Some(browse_guidance) =
        browse_tool_guidance(has_exec, has_search, has_list_files, has_read_file, shell_profile)
    {
        lines.push(browse_guidance);
    }
    if has_exec {
        lines.push("- In Planning workflow, use `exec_command` only for read-only verification.".to_string());
    }
    if has_exec || has_search || has_read_file || has_list_files {
        lines.push("- In Planning workflow, diagnose from evidence; never bypass safeguards.".to_string());
    }
    if has_search {
        lines.push("- `code_search`: omit unused filters; no empty values (`path: \"\"`).".to_string());
    }
    if has_task_tracker {
        lines.push("- Keep `task_tracker` updated as you refine the plan.".to_string());
        lines.push("- Keep blockers and verification open in `task_tracker` until resolved.".to_string());
        lines.push("- Use `task_tracker` action=update with positive flat indices or positive hierarchical index_path values; index: 0 is only for standard checklist-level completion with status=completed, and bulk updates use items.".to_string());
    }
    if has_request_user_input {
        lines.push(
            "- Use `request_user_input` only for material blockers that remain after repository exploration."
                .to_string(),
        );
    }
    if has_search || has_exec {
        lines.push("- If calls repeat without progress, tighten the plan instead of retrying identically.".to_string());
    }

    format!("\n\n## Active Tools\n{}", lines.join("\n"))
}

/// Return the snapshot's active tool names, deduplicated and sorted.
fn snapshot_tool_names(tool_snapshot: &SessionToolCatalogueSnapshot) -> Vec<String> {
    tool_snapshot
        .active_tool_names
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn browse_tool_guidance(
    has_exec: bool,
    has_search: bool,
    has_list_files: bool,
    has_read_file: bool,
    shell_profile: ResolvedShellPromptProfile,
) -> Option<String> {
    if has_exec {
        return Some(shell_browse_guidance(shell_profile).to_string());
    }

    if !(has_search || has_list_files || has_read_file) {
        return None;
    }

    Some("- Use available read-only repository tools for browsing; do not modify files.".to_string())
}

pub fn render_shell_profile_guidance(shell_profile: ResolvedShellPromptProfile) -> String {
    match shell_profile {
        ResolvedShellPromptProfile::UnixLike => {
            "## Shell Profile\n- Active shell profile: `unix_like`. Use Unix-like command syntax in `exec_command.cmd`, for example `ls`, `rg`, `find`, `cat`, `sed`, and `awk`.\n- On macOS, write BSD-compatible flags for BSD tools. Diapason does not rewrite GNU flags for macOS BSD tools.\n- The shell profile controls prompt examples and expected command syntax only; command policy, sandboxing, and approvals remain separate runtime checks.\n- Diapason does not translate GNU-to-BSD, BSD-to-GNU, Unix-to-PowerShell, or PowerShell-to-Unix command flags.".to_string()
        }
        ResolvedShellPromptProfile::PowerShell => {
            "## Shell Profile\n- Active shell profile: `powershell`. Use native PowerShell syntax in `exec_command.cmd`, for example `Get-ChildItem`, `Select-String`, `Get-Content`, and `Where-Object`.\n- On native Windows, use WSL when you need Unix-like workflows or Unix command examples.\n- The shell profile controls prompt examples and expected command syntax only; command policy, sandboxing, and approvals remain separate runtime checks.\n- Diapason does not translate GNU-to-BSD, BSD-to-GNU, Unix-to-PowerShell, or PowerShell-to-Unix command flags.".to_string()
        }
    }
}

fn shell_browse_guidance(shell_profile: ResolvedShellPromptProfile) -> &'static str {
    match shell_profile {
        ResolvedShellPromptProfile::UnixLike => {
            "- Browse: `exec_command.cmd`: `ls`, `rg`, `find`, `cat`, `sed`, `awk`."
        }
        ResolvedShellPromptProfile::PowerShell => {
            "- Use `exec_command.cmd` with native PowerShell commands such as `Get-ChildItem`, `Select-String`, `Get-Content`, and `Where-Object` for repository browsing."
        }
    }
}

/// Returns the shell-task instruction for the active shell profile.
fn shell_task_guidance(shell_profile: ResolvedShellPromptProfile) -> &'static str {
    match shell_profile {
        ResolvedShellPromptProfile::UnixLike => {
            "- Use `exec_command.cmd`: build tools, test tools, `git diff -- <path>`, shell tasks. One-shot `exec_command` calls: do not use `!!`, `!$`, `!ssh`, or `fc`; write full command arguments explicitly from conversation or tool results. Interactive shells: use review-safe history expansion (Bash `histverify`, zsh `HIST_VERIFY`)."
        }
        ResolvedShellPromptProfile::PowerShell => {
            "- Use `exec_command.cmd` for build tools, test tools, `git diff -- <path>`, and shell-only tasks using native PowerShell syntax."
        }
    }
}

fn read_only_batching_guidance(has_read_file: bool) -> &'static str {
    if has_read_file {
        "- Batch independent read-only calls; bound `read_file` ranges; order dependencies; serialize mutations; `line_truncated`: small/byte ranges."
    } else {
        "- Batch independent read-only calls; order dependent reads, and serialize mutations."
    }
}

/// Returns advanced `code_search` guidance, including syntax help when execution is available.
fn code_search_guidance(has_exec: bool, shell_profile: ResolvedShellPromptProfile) -> &'static str {
    match (has_exec, shell_profile) {
        (true, _) => {
            "- Advanced `code_search` takes `query`; omit unused filters; no empty values (`path: \"\"`). Filters `path`/`file_types`/`result_types`/`max_results`; returns definitions/exact syntactic usages; literal smart-case, `|`-separated literals; narrow truncation. E.g. `{\"query\":\"TurnLoop\",\"path\":\"src\",\"result_types\":[\"definition\"],\"max_results\":20}`; result_types/max_results: array/integer; Do not JSON-encode arrays or integers as strings; use `exec_command` or skill for syntax patterns."
        }
        (false, _) => {
            "- Advanced `code_search` takes `query`; omit unused filters; no empty values (`path: \"\"`). Filters `path`/`file_types`/`result_types`/`max_results`; returns definitions/exact syntactic usages; literal smart-case, `|`-separated literals; narrow truncation. E.g. `{\"query\":\"TurnLoop\",\"path\":\"src\",\"result_types\":[\"definition\"],\"max_results\":20}`; result_types/max_results: array/integer; Do not JSON-encode arrays or integers as strings."
        }
    }
}

fn capability_mode_line(
    capability_level: Option<CapabilityLevel>,
    has_exec: bool,
    has_file: bool,
) -> Option<&'static str> {
    match capability_level {
        Some(CapabilityLevel::Basic) => {
            Some("- Capabilities: limited. Ask the user to enable more capabilities if file work is required.")
        }
        Some(CapabilityLevel::FileReading | CapabilityLevel::FileListing) => {
            Some("- Capabilities: read-only. Analyse and search, but do not modify files or run shell commands.")
        }
        _ if !has_exec && !has_file => {
            Some("- Capabilities: read-only. Analyse and search, but do not modify files or run shell commands.")
        }
        _ => None,
    }
}

/// Infer capability level from available tools.
pub fn infer_capability_level(available_tools: &[String]) -> CapabilityLevel {
    let has_search = available_tools.iter().any(|t| t == TOOL_CODE_SEARCH);
    let has_edit = available_tools.iter().any(|t| t == TOOL_APPLY_PATCH);
    let has_read = has_edit || available_tools.iter().any(|t| t == TOOL_READ_FILE);
    let has_list = has_search || available_tools.iter().any(|t| t == TOOL_LIST_FILES);
    let has_exec = available_tools.iter().any(|t| t == TOOL_EXEC_COMMAND);

    if has_search {
        CapabilityLevel::CodeSearch
    } else if has_edit {
        CapabilityLevel::Editing
    } else if has_exec {
        CapabilityLevel::Bash
    } else if has_list {
        CapabilityLevel::FileListing
    } else if has_read {
        CapabilityLevel::FileReading
    } else {
        CapabilityLevel::Basic
    }
}

#[cfg(test)]
#[path = "guidelines_tests.rs"]
mod tests;
