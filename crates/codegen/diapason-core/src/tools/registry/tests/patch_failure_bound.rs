//! Repeat-bound coverage for failing apply-patch calls.
//!
//! The generic identical-call detector is bypassed for `apply_patch`, and the
//! no-op ladder only observes calls that reach planning. These tests pin the
//! dedicated failure ladder for rejections that return earlier, and check that
//! it never blocks a materially different request.

use super::*;

/// Identical version-precondition failures are bounded after two attempts.
#[tokio::test]
async fn repeated_identical_precondition_failures_are_blocked() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path().join("stale.txt");
    fs::write(&path, "current\n")?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;

    let patch = "*** Begin Patch\n*** Update File: stale.txt\n@@\n-current\n+changed\n*** End Patch\n";
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"a different revision\n"));
    let request = json!({ "input": patch, "expected_content_hash": stale_hash });

    for attempt in 1..=2 {
        let rejected = registry.execute_tool(tools::APPLY_PATCH, request.clone()).await?;
        assert_eq!(
            rejected["error"]["details"]["reason"], "content_hash_mismatch",
            "attempt {attempt} must reach the precondition: {rejected:?}"
        );
    }

    let blocked = registry.execute_tool(tools::APPLY_PATCH, request).await?;
    assert_eq!(
        blocked["error"]["details"]["reason"], "repeated_identical_failure",
        "the third identical failure must be refused: {blocked:?}"
    );
    assert_eq!(blocked["error"]["details"]["occurrence"], 3);
    assert_eq!(blocked["error"]["details"]["retry_prohibited"], true);
    assert!(
        !blocked.to_string().contains("stale.txt"),
        "the refusal must not echo patch text or paths: {blocked:?}"
    );
    assert_eq!(fs::read_to_string(&path)?, "current\n", "a refused patch must not mutate the file");
    Ok(())
}

/// A refusal must not block a materially different request.
#[tokio::test]
async fn a_different_request_after_a_refusal_is_still_attempted() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path().join("stale.txt");
    fs::write(&path, "current\n")?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;

    let patch = "*** Begin Patch\n*** Update File: stale.txt\n@@\n-current\n+changed\n*** End Patch\n";
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"a different revision\n"));
    let stale_request = json!({ "input": patch, "expected_content_hash": stale_hash });
    for _ in 0..2 {
        registry.execute_tool(tools::APPLY_PATCH, stale_request.clone()).await?;
    }
    let blocked = registry.execute_tool(tools::APPLY_PATCH, stale_request).await?;
    assert_eq!(blocked["error"]["details"]["reason"], "repeated_identical_failure");

    let other_patch = "*** Begin Patch\n*** Update File: stale.txt\n@@\n-current\n+other\n*** End Patch\n";
    let other = registry
        .execute_tool(tools::APPLY_PATCH, json!({ "input": other_patch, "expected_content_hash": stale_hash }))
        .await?;
    assert_eq!(
        other["error"]["details"]["reason"], "content_hash_mismatch",
        "a different request starts its own ladder: {other:?}"
    );

    let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"current\n"));
    let applied = registry
        .execute_tool(tools::APPLY_PATCH, json!({ "input": other_patch, "expected_content_hash": current_hash }))
        .await?;
    assert_eq!(applied["success"], true, "a corrected request must still apply: {applied:?}");
    assert_eq!(fs::read_to_string(&path)?, "other\n");
    Ok(())
}

/// Failures raised before the patch is parsed are bounded too.
#[tokio::test]
async fn repeated_decode_failures_are_blocked() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;
    let request = json!({ "input": "base64:not-valid-base64!!" });

    for attempt in 1..=2 {
        let rejected = registry.execute_tool(tools::APPLY_PATCH, request.clone()).await?;
        let message = rejected["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("Failed to decode base64 patch"),
            "attempt {attempt} must reach the decoder: {rejected:?}"
        );
    }

    let blocked = registry.execute_tool(tools::APPLY_PATCH, request).await?;
    assert_eq!(
        blocked["error"]["details"]["reason"], "repeated_identical_failure",
        "decode failures return before planning and still need a bound: {blocked:?}"
    );
    assert_eq!(blocked["error"]["details"]["occurrence"], 3);
    Ok(())
}
