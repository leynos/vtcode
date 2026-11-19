//! Background record ids and the managed-subagent launch command.
use super::*;

#[test]
fn background_record_ids_are_stable_and_sanitized() {
    assert_eq!(background_record_id("Rust Engineer"), "background-Rust-Engineer");
    assert_eq!(background_record_id("plugin:reviewer/default"), "background-plugin-reviewer-default");
}

#[test]
fn background_subagent_command_includes_expected_flags() {
    let workspace = std::env::current_dir().expect("workspace");
    let command = build_background_subagent_command(
        &workspace,
        "rust-engineer",
        "session-parent",
        "session-child",
        "Inspect the repo",
        Some(7),
        Some("gpt-5.6-luna"),
        Some("high"),
    )
    .expect("background command");

    assert!(command.len() >= 15);
    assert_eq!(command[1], "background-subagent");
    assert!(command.windows(2).any(|pair| pair == ["--agent-name", "rust-engineer"]));
    assert!(command.windows(2).any(|pair| pair == ["--parent-session-id", "session-parent"]));
    assert!(command.windows(2).any(|pair| pair == ["--session-id", "session-child"]));
    assert!(command.windows(2).any(|pair| pair == ["--prompt", "Inspect the repo"]));
    assert!(command.windows(2).any(|pair| pair == ["--max-turns", "7"]));
    assert!(command.windows(2).any(|pair| pair == ["--model-override", "gpt-5.6-luna"]));
    assert!(command.windows(2).any(|pair| pair == ["--reasoning-override", "high"]));
}
