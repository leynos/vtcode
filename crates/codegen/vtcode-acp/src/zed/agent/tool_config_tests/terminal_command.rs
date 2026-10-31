//! Tests terminal command argument parsing and working-directory resolution.
use super::super::ZedAgent;
use super::support::build_agent;
use assert_fs::TempDir;
use serde_json::json;

#[test]
fn parse_terminal_command_rejects_empty_array() {
    let args = json!({ "command": [] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command array cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_empty_string() {
    let args = json!({ "command": "" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command string cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_whitespace_only_string() {
    let args = json!({ "command": "   " });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command string cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_empty_executable_in_array() {
    let args = json!({ "command": ["", "arg1", "arg2"] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command executable cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_whitespace_only_executable_in_array() {
    let args = json!({ "command": ["  ", "arg1"] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command executable cannot be empty");
}

#[test]
fn parse_terminal_command_accepts_valid_array() {
    let args = json!({ "command": ["ls", "-la"] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["ls", "-la"]);
}

#[test]
fn parse_terminal_command_accepts_valid_string() {
    let args = json!({ "command": "echo test" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["echo", "test"]);
}

#[test]
fn parse_terminal_command_accepts_cmd_alias() {
    let args = json!({ "cmd": "echo test" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["echo", "test"]);
}

#[test]
fn parse_terminal_command_rejects_missing_command_field() {
    let args = json!({});
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "command execution requires a 'command' field (string/array or indexed command.N entries)"
    );
}

#[test]
fn parse_terminal_command_accepts_indexed_arguments_zero_based() {
    let args = json!({ "command.0": "python", "command.1": "-c", "command.2": "print('hi')" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["python", "-c", "print('hi')"]);
}

#[test]
fn parse_terminal_command_accepts_indexed_arguments_one_based() {
    let args = json!({ "command.1": "ls", "command.2": "-a" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["ls", "-a"]);
}

#[test]
fn parse_terminal_command_rejects_non_string_indexed_argument() {
    let args = json!({ "command.0": 1 });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command array must contain only strings");
}

#[tokio::test]
async fn resolve_terminal_working_dir_accepts_workdir_alias() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let args = json!({ "workdir": "src" });

    let working_dir = agent
        .resolve_terminal_working_dir(&args)
        .expect("workdir alias should resolve")
        .expect("working directory should be present");

    assert_eq!(working_dir, temp.path().join("src"));
}
