//! Child runtime configuration, hooks, model view and turn budgets.
use super::*;

/// The built-in `worker` sub-agent, which every case below adjusts in place.
///
/// Each test overrides only the fields it exercises, so the shared lookup lives
/// here rather than being repeated in every case.
fn worker_spec() -> SubagentSpec {
    diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "worker")
        .expect("the built-in worker sub-agent")
}

#[test]
fn build_child_config_intersects_allowed_tools_and_preserves_global_denies() {
    let mut parent = DiapasonConfig::default();
    parent.permissions.allow = vec![tools::READ_FILE.to_string(), tools::CODE_SEARCH.to_string()];
    parent.permissions.deny = vec![tools::UNIFIED_EXEC.to_string()];

    let mut spec = worker_spec();
    spec.permissions = AgentPermissionsConfig {
        auto: vec!["Bash(*)".to_string()],
        ..AgentPermissionsConfig::new(PermissionDefault::Auto)
    };
    spec.tools = Some(vec![
        tools::SPAWN_AGENT.to_string(),
        tools::CODE_SEARCH.to_string(),
        tools::READ_FILE.to_string(),
    ]);

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, false);
    assert_eq!(child.runtime_agent_permissions.as_ref(), Some(&spec.permissions));
    assert_eq!(child.permissions.allow, vec![tools::READ_FILE.to_string(), tools::CODE_SEARCH.to_string()]);
    assert!(child.permissions.deny.contains(&tools::UNIFIED_EXEC.to_string()));
    assert!(child.permissions.deny.contains(&tools::SPAWN_AGENT.to_string()));
}

#[test]
fn build_child_config_allows_nested_delegation_keeps_agent_tools_out_of_deny() {
    let mut parent = DiapasonConfig::default();
    parent.permissions.allow = vec![
        tools::SPAWN_AGENT.to_string(),
        tools::WAIT_AGENT.to_string(),
        tools::CODE_SEARCH.to_string(),
    ];

    let mut spec = worker_spec();
    spec.tools = Some(parent.permissions.allow.clone());

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, true);

    assert_eq!(
        child.permissions.allow,
        vec![
            tools::SPAWN_AGENT.to_string(),
            tools::WAIT_AGENT.to_string(),
            tools::CODE_SEARCH.to_string()
        ],
        "nested delegation keeps delegation tools in the allow-list"
    );
    assert!(
        !child.permissions.deny.contains(&tools::SPAWN_AGENT.to_string()),
        "spawn_agent must not be denied when nested delegation is allowed"
    );
    assert!(
        !child.permissions.deny.contains(&tools::AGENT.to_string()),
        "agent must not be denied when nested delegation is allowed"
    );
    assert!(
        child.permissions.deny.contains(&tools::SPAWN_BACKGROUND_SUBPROCESS.to_string()),
        "background subprocess alias stays blocked for children"
    );
}

#[test]
fn build_child_config_default_denies_all_subagent_tools() {
    let parent = DiapasonConfig::default();
    let spec = worker_spec();

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, false);

    assert!(child.permissions.deny.contains(&tools::SPAWN_AGENT.to_string()));
    assert!(child.permissions.deny.contains(&tools::AGENT.to_string()));
    assert!(child.permissions.deny.contains(&tools::SEND_INPUT.to_string()));
    assert!(child.permissions.deny.contains(&tools::WAIT_AGENT.to_string()));
    assert!(child.permissions.deny.contains(&tools::RESUME_AGENT.to_string()));
    assert!(child.permissions.deny.contains(&tools::CLOSE_AGENT.to_string()));
    assert!(child.permissions.deny.contains(&tools::SPAWN_BACKGROUND_SUBPROCESS.to_string()));
}

#[test]
fn build_child_config_preserves_subagent_lifecycle_stripping_and_hook_merging() {
    let mut parent = DiapasonConfig::default();
    parent.permissions.allow = vec![
        tools::SPAWN_AGENT.to_string(),
        tools::CODE_SEARCH.to_string(),
        tools::UNIFIED_EXEC.to_string(),
    ];

    let mut spec = worker_spec();
    spec.tools = Some(parent.permissions.allow.clone());
    let mut hooks = HooksConfig::default();
    hooks.lifecycle.pre_tool_use.push(HookGroupConfig {
        matcher: Some("*".to_string()),
        hooks: vec![HookCommandConfig {
            command: "echo child".to_string(),
            ..HookCommandConfig::default()
        }],
    });
    spec.hooks = Some(hooks);

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, false);

    assert_eq!(child.permissions.allow, vec![tools::CODE_SEARCH.to_string(), tools::UNIFIED_EXEC.to_string()]);
    assert!(child.permissions.deny.contains(&tools::SPAWN_AGENT.to_string()));
    assert_eq!(child.hooks.lifecycle.pre_tool_use.len(), 1);
    assert_eq!(child.hooks.lifecycle.pre_tool_use[0].hooks[0].command, "echo child");
}

#[test]
fn prepare_child_runtime_config_uses_shared_view_for_model_and_reasoning() {
    let parent = DiapasonConfig::default();
    let mut spec = worker_spec();
    spec.model = Some(models::openai::GPT_5_6_LUNA.to_string());
    spec.reasoning_effort = Some(ReasoningEffortLevel::High);

    let (resolved_model, child_reasoning_effort, child_cfg) = prepare_child_runtime_config(
        &parent,
        &spec,
        models::openai::GPT_5_6_SOL,
        "openai",
        ReasoningEffortLevel::Low,
        None,
        None,
        None,
        false,
        |_, parent_model, parent_provider, model_override, spec_model, agent_name| {
            assert_eq!(parent_model, models::openai::GPT_5_6_SOL);
            assert_eq!(parent_provider, "openai");
            assert_eq!(model_override, None);
            assert_eq!(spec_model, Some(models::openai::GPT_5_6_LUNA));
            assert_eq!(agent_name, "worker");
            Ok(models::openai::GPT_5_6_LUNA.parse::<ModelId>().expect("valid model"))
        },
    )
    .expect("prepared child runtime config");

    assert_eq!(resolved_model.as_str(), models::openai::GPT_5_6_LUNA);
    assert_eq!(child_cfg.agent.default_model, models::openai::GPT_5_6_LUNA);
    assert_eq!(child_reasoning_effort, ReasoningEffortLevel::High);
    assert_eq!(child_cfg.agent.reasoning_effort, ReasoningEffortLevel::High);
}

#[test]
fn subagent_instruction_composition_uses_shared_runtime_prompt_and_skill_appendix() {
    let mut spec = worker_spec();
    spec.prompt = "Worker instructions".to_string();
    spec.skills = vec!["rust".to_string(), "repo".to_string()];

    let instructions = compose_subagent_instructions(&spec, Some("Memory appendix".to_string()));

    assert!(instructions.contains("Worker instructions"));
    assert!(instructions.contains("Preloaded skill names: rust, repo."));
    assert!(instructions.contains("Memory appendix"));
    assert!(instructions.contains("Return your final response using this exact Markdown contract"));
}

#[test]
fn build_child_config_preserves_matching_rule_and_exact_tool_ids() {
    let mut parent = DiapasonConfig::default();
    parent.permissions.allow = vec![
        "Read(/docs/**)".to_string(),
        "mcp::context7::search".to_string(),
        tools::READ_FILE.to_string(),
    ];

    let mut spec = worker_spec();
    spec.tools = Some(vec![
        "mcp::context7::search".to_string(),
        tools::UNIFIED_EXEC.to_string(),
        tools::READ_FILE.to_string(),
    ]);

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, false);

    assert_eq!(
        child.permissions.allow,
        vec![
            "Read(/docs/**)".to_string(),
            "mcp::context7::search".to_string(),
            tools::READ_FILE.to_string()
        ]
    );
}

#[test]
fn build_child_config_preserves_parent_rule_shaped_allowlist() {
    let mut parent = DiapasonConfig::default();
    parent.permissions.allow = vec!["Read".to_string()];

    let mut spec = worker_spec();
    spec.tools = Some(vec![
        tools::READ_FILE.to_string(),
        tools::CODE_SEARCH.to_string(),
        tools::UNIFIED_EXEC.to_string(),
    ]);

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, false);

    assert_eq!(child.permissions.allow, vec!["Read".to_string()]);
}

#[test]
fn build_child_config_promotes_single_turn_budget_to_recovery_budget() {
    let parent = DiapasonConfig::default();
    let spec = worker_spec();

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, Some(1), false);

    assert_eq!(child.automation.full_auto.max_turns, SUBAGENT_MIN_MAX_TURNS);
}

#[test]
fn background_children_get_a_higher_turn_floor() {
    assert_eq!(normalize_background_child_max_turns(Some(2), true), Some(4));
    assert_eq!(normalize_background_child_max_turns(Some(3), true), Some(4));
    assert_eq!(normalize_background_child_max_turns(Some(4), true), Some(4));
}

#[test]
fn foreground_children_keep_the_existing_turn_floor() {
    assert_eq!(normalize_background_child_max_turns(Some(1), false), Some(SUBAGENT_MIN_MAX_TURNS));
    assert_eq!(normalize_background_child_max_turns(Some(2), false), Some(2));
    assert_eq!(normalize_background_child_max_turns(None, true), None);
}

#[test]
fn build_child_config_merges_inline_mcp_provider() {
    let parent = DiapasonConfig::default();
    let mut spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "default")
        .expect("default");
    spec.mcp_servers = vec![SubagentMcpServer::Inline(BTreeMap::from([(
        "playwright".to_string(),
        serde_json::json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@playwright/mcp@latest"],
        }),
    )]))];

    let child = build_child_config(&parent, &spec, models::openai::GPT_5_6_SOL, None, false);
    let provider = child
        .mcp
        .providers
        .iter()
        .find(|provider| provider.name == "playwright")
        .expect("playwright provider");
    assert_eq!(provider.name, "playwright");
}
