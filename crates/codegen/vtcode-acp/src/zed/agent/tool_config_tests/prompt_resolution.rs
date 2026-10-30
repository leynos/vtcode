//! Tests resolved session prompts for default and custom primary agents.
use super::support::build_agent;
use assert_fs::TempDir;
use std::fs;
use vtcode_core::llm::provider::MessageRole;

#[tokio::test]
async fn resolved_messages_include_custom_primary_agent_prompt() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/research.md"),
        r#"---
name: research
description: Research primary
mode: primary
permissions:
  default: deny
---
Research primary prompt."#,
    )
    .unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();
    assert!(agent.update_session_primary_agent(&session, "research".to_string()).unwrap());

    let messages = agent.resolved_messages(&session).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::System);
    assert_eq!(messages[0].content.as_text(), "Research primary prompt.");
}

#[tokio::test]
async fn resolved_messages_include_primary_agent_prompt() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    let messages = agent.resolved_messages(&session).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::System);
    let prompt = messages[0].content.as_text();
    assert!(prompt.contains("You are the duck agent"));
    assert!(!prompt.contains("Architect mode"));
    assert!(!prompt.contains("Code mode"));
}
