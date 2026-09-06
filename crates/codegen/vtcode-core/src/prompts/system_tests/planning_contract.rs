//! Planning contract and generated-prompt regressions.

use super::*;

/// Regression guard: `PLANNING_WORKFLOW_PLAN_QUALITY_LINE` must keep
/// instructing the model to write file:symbol references as plain text
/// / inline code, not as markdown links or editor/IDE URI schemes (a
/// model was observed emitting `vscode-file://` pseudo-links pointing at
/// the editor binary instead of the referenced repo file).
#[test]
fn plan_quality_line_forbids_markdown_link_file_references() {
    let line = PLANNING_WORKFLOW_PLAN_QUALITY_LINE;
    assert!(line.contains("never as markdown links or editor/IDE URIs"));
    assert!(line.contains("vscode-file://"));
    assert!(line.contains("plain text or inline code"));
}

/// The initial prompt must show the exact step grammar the validator
/// enforces, not just describe it — the repair directive prints the
/// canonical example only after a first rejection (turn_912/913 failed
/// every implementation step on "lacks a concrete target or verification"
/// without the model ever seeing an example). Keep the inline literal in
/// sync with the validator's canonical format.
#[test]
fn plan_quality_line_shows_canonical_step_format() {
    assert!(
        PLANNING_WORKFLOW_PLAN_QUALITY_LINE
            .contains(crate::tools::handlers::planning_workflow::artefacts::CANONICAL_STEP_FORMAT),
        "quality line must embed artefacts::CANONICAL_STEP_FORMAT verbatim"
    );
}

#[test]
fn plan_quality_line_requires_concrete_verify_checks() {
    let line = PLANNING_WORKFLOW_PLAN_QUALITY_LINE;
    assert!(line.contains("one concrete `verify:`/`verification:` command or observable check"));
    assert!(line.contains("vague prose"));
    assert!(line.contains("comma-separated verify entries that are not commands or observable checks"));
}

#[test]
fn planning_workflow_persistence_policy_assigns_plan_lifecycle_to_runtime() {
    let line = PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE;
    assert!(line.contains("Emit exactly one final `<proposed_plan>` block"));
    assert!(line.contains("no surrounding prose"));
    assert!(line.contains("Do not use shell commands or file-writing tools to create or modify `.vtcode/plans/`"));
    assert!(line.contains("runtime owns plan/tracker persistence and validation"));
    assert!(line.contains("approval controls only after successful persistence"));
}

#[test]
fn test_minimal_prompt_token_count() {
    // Rough estimate: 1 token ≈ 4 characters
    let approx_tokens = minimal_system_prompt().len() / 4;
    assert!(approx_tokens < 350, "Minimal prompt should stay compact, got ~{approx_tokens}");
}

#[test]
fn test_default_prompt_token_count() {
    let approx_tokens = default_system_prompt().len() / 4;
    assert!(approx_tokens < 700, "Default prompt should stay compact, got ~{approx_tokens}");
}

#[tokio::test]
async fn test_default_live_prompt_budget_with_instruction_inline() {
    use crate::project_doc::build_instruction_appendix_with_context;

    let workspace = tempfile::TempDir::new().expect("workspace");
    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "- run ./scripts/check.sh\n- avoid adding to vtcode-core\n- use Conventional Commits\n- start with docs/ARCHITECTURE.md\n",
    )
    .expect("write agents");
    std::fs::create_dir_all(workspace.path().join(".vtcode/rules")).expect("rules dir");
    std::fs::write(
        workspace.path().join(".vtcode/rules/rust.md"),
        "---\npaths:\n  - \"**/*.rs\"\n---\n# Rust\n- keep changes surgical\n",
    )
    .expect("write rust rule");
    std::fs::create_dir_all(workspace.path().join("src")).expect("src dir");
    std::fs::write(workspace.path().join("src/lib.rs"), "pub fn main() {}\n").expect("write lib.rs");

    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    let base = compose_system_instruction_text(workspace.path(), Some(&config), None).await;
    let appendix = build_instruction_appendix_with_context(
        &config.agent,
        workspace.path(),
        &[workspace.path().join("src/lib.rs")],
    )
    .await
    .expect("instruction appendix");
    let prompt = format!("{base}\n\n# INSTRUCTIONS\n{appendix}");
    let approx_tokens = prompt.len() / 4;

    assert!(prompt.contains("### Instruction map"));
    assert!(prompt.contains("# Rust"));
    assert!(prompt.contains("- keep changes surgical"));
    assert!(!prompt.contains("### On-demand loading"));
    assert!(approx_tokens <= 1250, "got ~{approx_tokens} tokens");
}

#[tokio::test]
async fn test_generated_prompts_do_not_use_deprecated_update_plan() {
    let project_root = PathBuf::from(".");

    for (mode_name, mode) in [
        ("default", SystemPromptMode::Default),
        ("minimal", SystemPromptMode::Minimal),
        ("specialized", SystemPromptMode::Specialized),
    ] {
        let mut config = VTCodeConfig::default();
        config.agent.system_prompt_mode = mode;
        config.agent.include_temporal_context = false;
        config.agent.include_working_directory = false;
        config.agent.instruction_max_bytes = 0;

        let result = compose_system_instruction_text(&project_root, Some(&config), None).await;

        assert!(!result.contains("update_plan"), "{mode_name} prompt should not reference deprecated update_plan");
    }
}

#[tokio::test]
async fn test_default_prompt_omits_non_baseline_tools() {
    let project_root = PathBuf::from(".");
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Default;
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    let result = compose_system_instruction_text(&project_root, Some(&config), None).await;

    assert!(result.contains("`exec_command`, `write_stdin`, and `apply_patch`"));
    assert!(result.contains("exec_command.cmd"));
    assert!(result.contains("## Shell Profile"));
    assert!(!result.contains("task_tracker"));
    assert!(!result.contains("list_files"));
    assert!(!result.contains("read_file"));
}

#[tokio::test]
async fn test_default_and_specialized_prompts_drop_rigid_summary_template() {
    let project_root = PathBuf::from(".");

    for (mode_name, mode) in [
        ("default", SystemPromptMode::Default),
        ("specialized", SystemPromptMode::Specialized),
    ] {
        let mut config = VTCodeConfig::default();
        config.agent.system_prompt_mode = mode;
        config.agent.include_temporal_context = false;
        config.agent.include_working_directory = false;
        config.agent.instruction_max_bytes = 0;

        let result = compose_system_instruction_text(&project_root, Some(&config), None).await;

        assert!(!result.contains("References\n"), "{mode_name} prompt should not force a References section");
        assert!(!result.contains("Next action"), "{mode_name} prompt should not force a Next action section");
        assert!(
            !result.contains("Scope checkpoint"),
            "{mode_name} prompt should not require the old plan blueprint bullets"
        );
    }
}

#[tokio::test]
async fn test_generated_prompts_keep_sparse_execution_contract() {
    let project_root = PathBuf::from(".");

    for (mode_name, mode) in [
        ("default", SystemPromptMode::Default),
        ("minimal", SystemPromptMode::Minimal),
        ("lightweight", SystemPromptMode::Lightweight),
        ("specialized", SystemPromptMode::Specialized),
    ] {
        let mut config = VTCodeConfig::default();
        config.agent.system_prompt_mode = mode;
        config.agent.include_temporal_context = false;
        config.agent.include_working_directory = false;
        config.agent.instruction_max_bytes = 0;

        let result = compose_system_instruction_text(&project_root, Some(&config), None).await;
        let normalized = result.to_ascii_lowercase();

        assert!(
            normalized.contains("compact") || normalized.contains("concise"),
            "{mode_name} prompt should keep output guidance compact"
        );
        assert!(
            normalized.contains("low-risk") || normalized.contains("reversible"),
            "{mode_name} prompt should include follow-through guidance"
        );
        assert!(
            normalized.contains("verify") || normalized.contains("validation"),
            "{mode_name} prompt should include verification guidance"
        );
        assert!(normalized.contains("do not guess"), "{mode_name} prompt should gate missing context");
        assert!(
            normalized.contains("unblocked portion")
                || normalized.contains("unblocked slices")
                || normalized.contains("answerable without a missing detail"),
            "{mode_name} prompt should require partial progress before clarification"
        );
        assert!(
            normalized.contains("before tools: state the next phase in one line")
                && normalized.contains("standalone recap (found, changed, verified, next)"),
            "{mode_name} prompt should define user-facing progress updates"
        );
        assert!(
            normalized.contains("retrieved sources") || normalized.contains("retrieved evidence"),
            "{mode_name} prompt should include grounding/citation guidance"
        );
        assert!(!result.contains('ƒ'), "{mode_name} prompt should not contain stray prompt characters");
    }
}
