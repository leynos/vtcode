//! `ApplyPatchRuntime::run` behaviour when driven directly.
//!
//! These bypass `ApplyPatchHandler::handle` and call the runtime, so the
//! workspace-containment check and the immediate recheck before the write are
//! covered on their own rather than through the orchestrator.

use super::*;

/// Check a symlinked cwd is rejected before the runtime mutates anything.
#[cfg(unix)]
#[tokio::test]
async fn direct_runtime_rejects_symlink_escaped_cwd_before_mutation() {
    use std::os::unix::fs::symlink;

    let workspace = TempDir::new().expect("workspace should be created");
    let outside = TempDir::new().expect("outside directory should be created");
    let escaped_cwd = workspace.path().join("escaped");
    symlink(outside.path(), &escaped_cwd).expect("cwd symlink should be created");

    let mut tool_ctx = direct_runtime_ctx(&workspace);
    tool_ctx.turn = Arc::new(TurnContext {
        cwd: escaped_cwd.clone(),
        turn_id: "direct-apply-patch-test".to_string(),
        sub_id: None,
        shell_environment_policy: ShellEnvironmentPolicy::default(),
        approval_policy: Constrained::default(),
        codex_linux_sandbox_exe: None,
        sandbox_policy: Constrained::default(),
    });
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: "*** Begin Patch\n*** Add File: created.txt\n+must not exist\n*** End Patch\n".to_string(),
        expected_content_hash: None,
        cwd: escaped_cwd,
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    let error = ApplyPatchRuntime::new()
        .run(&request, &attempt, &tool_ctx)
        .await
        .expect_err("direct apply_patch must reject an escaped cwd");
    assert!(error.to_string().contains("outside session workspace"));
    assert!(!outside.path().join("created.txt").exists());
}

/// The runtime must re-check `expected_content_hash` immediately before it
/// writes, so a change after the handler's check cannot be applied.
#[tokio::test]
async fn direct_runtime_rejects_stale_expected_content_hash_before_apply() {
    let workspace = TempDir::new().expect("workspace should be created");
    let fixture = workspace.path().join("versioned.txt");
    std::fs::write(&fixture, "current\n").expect("write fixture");
    let tool_ctx = direct_runtime_ctx(&workspace);
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n".to_string(),
        expected_content_hash: Some(format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"))),
        cwd: workspace.path().to_path_buf(),
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    let error = ApplyPatchRuntime::new()
        .run(&request, &attempt, &tool_ctx)
        .await
        .expect_err("the runtime must reject a stale expected_content_hash");
    assert!(
        matches!(&error, ToolError::RespondToModel(_)),
        "the recheck must keep the structured envelope model-facing, got {error:?}"
    );
    assert!(error.to_string().contains("content_hash_mismatch"), "unexpected error: {error}");
    assert_eq!(
        std::fs::read_to_string(&fixture).expect("read unchanged fixture"),
        "current\n",
        "a rejected precondition must not mutate the file"
    );
}

/// A matching hash must still apply through the direct runtime, proving the
/// recheck is a precondition rather than a blanket rejection.
#[tokio::test]
async fn direct_runtime_applies_patch_with_matching_expected_content_hash() {
    let workspace = TempDir::new().expect("workspace should be created");
    let fixture = workspace.path().join("versioned.txt");
    std::fs::write(&fixture, "current\n").expect("write fixture");
    let tool_ctx = direct_runtime_ctx(&workspace);
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n".to_string(),
        expected_content_hash: Some(format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"current\n"))),
        cwd: workspace.path().to_path_buf(),
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    ApplyPatchRuntime::new()
        .run(&request, &attempt, &tool_ctx)
        .await
        .expect("a matching content hash must allow the patch to apply");
    assert_eq!(
        std::fs::read_to_string(&fixture).expect("read patched fixture"),
        "changed\n",
        "the runtime should have applied the patch"
    );
}
