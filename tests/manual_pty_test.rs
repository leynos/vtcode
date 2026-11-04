#![allow(
    missing_docs,
    clippy::unwrap_used,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
//! Manual test for PTY functionality

use anyhow::Result;
use assert_fs::TempDir;
use diapason_core::tools::ToolRegistry;
use serde_json::{Value, json};

#[tokio::main]
#[expect(clippy::panic_in_result_fn, reason = "test function, assertions are expected")]
async fn main() -> Result<()> {
    println!("Testing PTY functionality...");

    // Create a temporary directory for testing
    let temp_dir = TempDir::new()?;
    let workspace = temp_dir.path().to_path_buf();
    let registry = ToolRegistry::new(workspace.clone()).await;

    // Test 1: Basic PTY command
    println!("\n=== Test 1: Basic PTY command ===");
    let args = json!({
        "command": "echo",
        "args": ["Hello, PTY!"]
    });

    match registry
        .execute_tool(diapason_core::config::constants::tools::RUN_PTY_CMD, args)
        .await
    {
        Ok(result) => {
            println!("Success: {result:?}");
            assert_eq!(result.get("success"), Some(&Value::Bool(true)));
            assert_eq!(result.get("code").and_then(Value::as_i64), Some(0));
            assert!(
                result
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("Hello, PTY!")
            );
            println!("✓ Test 1 passed");
        }
        Err(e) => {
            println!("Error: {e}");
            return Err(e);
        }
    }

    // Test 2: PTY command with working directory
    println!("\n=== Test 2: PTY command with working directory ===");

    // Create a test file
    std::fs::write(workspace.join("test.txt"), "Hello, PTY from file!")?;

    let args = json!({
        "command": "cat",
        "args": ["test.txt"]
    });

    match registry
        .execute_tool(diapason_core::config::constants::tools::RUN_PTY_CMD, args)
        .await
    {
        Ok(result) => {
            println!("Success: {result:?}");
            assert_eq!(result.get("success"), Some(&Value::Bool(true)));
            assert_eq!(result.get("code").and_then(Value::as_i64), Some(0));
            assert!(
                result
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("Hello, PTY from file!")
            );
            println!("✓ Test 2 passed");
        }
        Err(e) => {
            println!("Error: {e}");
            return Err(e);
        }
    }

    // Test 3: PTY session management
    println!("\n=== Test 3: PTY session management ===");

    // Create a PTY session
    let args = json!({
        "session_id": "test_session",
        "command": "bash"
    });

    match registry.execute_tool("create_pty_session", args).await {
        Ok(result) => {
            println!("Create session result: {result:?}");
            assert_eq!(result.get("success"), Some(&Value::Bool(true)));
            assert_eq!(result.get("session_id").and_then(Value::as_str), Some("test_session"));
            println!("✓ Session created");
        }
        Err(e) => {
            println!("Error creating session: {e}");
            return Err(e);
        }
    }

    // List PTY sessions
    let args = json!({});
    match registry.execute_tool("list_pty_sessions", args).await {
        Ok(result) => {
            println!("List sessions result: {result:?}");
            assert!(
                result
                    .get("sessions")
                    .and_then(Value::as_array)
                    .is_some_and(|sessions| sessions.contains(&Value::String("test_session".to_string())))
            );
            println!("✓ Session listed");
        }
        Err(e) => {
            println!("Error listing sessions: {e}");
            return Err(e);
        }
    }

    // Close PTY session
    let args = json!({
        "session_id": "test_session"
    });

    match registry.execute_tool("close_pty_session", args).await {
        Ok(result) => {
            println!("Close session result: {result:?}");
            assert_eq!(result.get("success"), Some(&Value::Bool(true)));
            assert_eq!(result.get("session_id").and_then(Value::as_str), Some("test_session"));
            println!("✓ Session closed");
        }
        Err(e) => {
            println!("Error closing session: {e}");
            return Err(e);
        }
    }

    println!("\n=== All tests passed! ===");
    Ok(())
}
