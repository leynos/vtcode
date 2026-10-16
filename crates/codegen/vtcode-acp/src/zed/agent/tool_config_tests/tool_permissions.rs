//! Tests primary-agent permissions as filters over the ACP local tool catalog.
use super::support::{build_agent, definition_names};
use crate::zed::helpers::PrimaryAgentCatalog;
use crate::zed::types::ToolRuntime;
use assert_fs::TempDir;
use std::fs;
use vtcode_core::config::constants::tools;

#[tokio::test]
async fn custom_primary_agent_permissions_control_local_tools() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/sheller.md"),
        r#"---
name: sheller
description: Shell primary
mode: primary
permissions:
  default: deny
  allow:
    - exec_command
---
Shell prompt."#,
    )
    .unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/reader.md"),
        r#"---
name: reader
description: Reader primary
mode: primary
permissions:
  default: deny
---
Reader prompt."#,
    )
    .unwrap();
    let agent = build_agent(temp.path()).await;
    let enabled_tools: Vec<_> = agent
        .tool_availability(true, false)
        .into_iter()
        .filter_map(|(tool, runtime)| match runtime {
            ToolRuntime::Enabled => Some(tool),
            ToolRuntime::Disabled => None,
        })
        .collect();

    let sheller_names = definition_names(agent.tool_definitions(true, &enabled_tools, "sheller").unwrap());
    let reader_names = definition_names(agent.tool_definitions(true, &enabled_tools, "reader").unwrap());

    assert!(sheller_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(!sheller_names.contains(&tools::APPLY_PATCH.to_string()));
    assert!(!reader_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(!reader_names.contains(&tools::APPLY_PATCH.to_string()));
}

#[tokio::test]
async fn allows_tool_gates_deny_default_agent_by_tool_category() {
    use vtcode_core::tools::names::canonical_tool_name;

    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let local_names = definition_names(agent.acp_tool_registry.definitions_for(&[], true));
    assert!(!local_names.is_empty(), "expected at least one local tool definition");

    // A `default: deny` agent that only allows `exec_command` must expose
    // exec_command and nothing that maps to a different, unallowed category.
    // This exercises `allows_tool` for every local tool name so name drift that
    // would silently over-permit (an unrecognized name falling through to an
    // always-permitted `Other` request) is caught.
    let catalog =
        PrimaryAgentCatalog::from_specs_with_default(&[deny_default_agent_allowing("exec_command")], "sheller");
    let workspace = temp.path();

    for name in &local_names {
        let canonical = canonical_tool_name(name).to_ascii_lowercase();
        let allowed = catalog.allows_tool("sheller", &canonical, workspace);
        if canonical == tools::EXEC_COMMAND {
            assert!(allowed, "explicitly allowed exec_command must be permitted");
        } else if canonical == tools::CODE_SEARCH || canonical == tools::WRITE_STDIN {
            // Read-only search and session-stdin are intentionally category-Other
            // / read tools whose deny-default handling is asserted elsewhere; skip
            // to avoid over-constraining their policy here.
        } else {
            assert!(
                !allowed,
                "deny-default agent must not expose '{name}' (canonical '{canonical}') \
                 when only exec_command is allowed"
            );
        }
    }
}
fn deny_default_agent_allowing(tool: &str) -> vtcode_config::SubagentSpec {
    use vtcode_config::core::permissions::PermissionDefault;
    let mut spec = vtcode_config::builtin_primary_build_agent();
    spec.name = "sheller".to_string();
    spec.description = "Shell primary".to_string();
    spec.permissions.default = PermissionDefault::Deny;
    spec.permissions.allow = vec![tool.to_string()];
    spec.permissions.ask = Vec::new();
    spec.permissions.auto = Vec::new();
    spec.permissions.deny = Vec::new();
    spec
}
