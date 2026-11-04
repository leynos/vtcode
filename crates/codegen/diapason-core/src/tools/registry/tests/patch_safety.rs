//! Version-precondition and no-op-ladder coverage for the registry's apply-patch surface.

use super::*;

/// An isolated workspace with a registry that permits every tool.
struct PatchWorkspace {
    registry: ToolRegistry,
    _temp_dir: TempDir,
}

/// Build one isolated registry for a patch-safety test.
#[fixture]
async fn patch_workspace() -> Result<PatchWorkspace> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;
    Ok(PatchWorkspace { registry, _temp_dir: temp_dir })
}

/// Check a stale hash returns diagnostics and leaves the file untouched.
#[rstest]
#[tokio::test]
async fn stale_apply_patch_hash_returns_diagnostics_without_writing(
    #[future] patch_workspace: Result<PatchWorkspace>,
) -> Result<()> {
    let PatchWorkspace { registry, _temp_dir: temp_dir } = patch_workspace.await?;
    let path = temp_dir.path().join("versioned.txt");
    fs::write(&path, "current\n")?;
    let patch = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n";
    let expected = format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"stale\n"));

    let outcome = registry
        .execute_public_tool_request(ToolExecutionRequest::new(
            tools::APPLY_PATCH,
            json!({ "input": patch, "expected_content_hash": expected }),
        ))
        .await;

    assert!(!outcome.is_success());
    let error = outcome.error.expect("hash mismatch error");
    assert_eq!(error.error_type, ToolErrorType::InvalidParameters);
    let details = error.details.expect("structured mismatch details");
    assert_eq!(details["reason"], "content_hash_mismatch");
    assert_eq!(details["expected_content_hash"], expected);
    assert_eq!(
        details["current_content_hash"],
        format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"current\n"))
    );
    assert_eq!(details["can_safely_rebase"], true);
    assert!(
        details["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("Reread versioned.txt"))
    );
    let records = registry.get_recent_tool_records(1);
    let recorded_error = records
        .first()
        .and_then(|record| record.result.as_ref().err())
        .expect("structured mismatch is retained in execution history");
    let recorded_error: Value = serde_json::from_str(recorded_error)?;
    assert_eq!(recorded_error["details"]["reason"], "content_hash_mismatch");
    assert_eq!(recorded_error["details"]["expected_content_hash"], expected);
    assert_eq!(fs::read_to_string(path)?, "current\n");
    Ok(())
}

/// Check an unrebasable stale patch reports bounded anchor excerpts.
#[rstest]
#[tokio::test]
async fn stale_unrebasable_patch_reports_bounded_anchor_context(
    #[future] patch_workspace: Result<PatchWorkspace>,
) -> Result<()> {
    let PatchWorkspace { registry, _temp_dir: temp_dir } = patch_workspace.await?;
    let path = temp_dir.path().join("versioned.txt");
    fs::write(&path, "fn nearby() {\n    current();\n}\n")?;
    let patch = "*** Begin Patch\n*** Update File: versioned.txt\n@@ fn missing()\n-    stale();\n+    changed();\n*** End Patch\n";
    let expected =
        format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"fn missing() {\n    stale();\n}\n"));

    let outcome = registry
        .execute_public_tool_request(ToolExecutionRequest::new(
            tools::APPLY_PATCH,
            json!({ "input": patch, "expected_content_hash": expected }),
        ))
        .await;

    let details = outcome.error.and_then(|error| error.details).expect("mismatch details");
    assert_eq!(details["can_safely_rebase"], false);
    let failure = details["anchor_failures"]
        .as_array()
        .and_then(|failures| failures.first())
        .expect("anchor failure");
    assert_eq!(failure["anchor"], "fn missing()");
    assert_eq!(failure["reason"], "missing_anchor");
    assert!(
        failure["expected_excerpt"]
            .as_str()
            .is_some_and(|excerpt| excerpt.contains("stale"))
    );
    assert!(
        failure["current_excerpt"]
            .as_str()
            .is_some_and(|excerpt| excerpt.contains("current"))
    );
    assert!(
        failure["expected_excerpt"]
            .as_str()
            .is_some_and(|excerpt| excerpt.chars().count() <= 240)
    );
    assert!(
        failure["current_excerpt"]
            .as_str()
            .is_some_and(|excerpt| excerpt.chars().count() <= 240)
    );
    assert_eq!(fs::read_to_string(path)?, "fn nearby() {\n    current();\n}\n");
    Ok(())
}

/// Check every excerpt and the anchor are bounded for oversized failing hunks.
///
/// The payload is model-visible, so an arbitrarily long anchor or line must not
/// reach the transcript, and the failure list must stay capped even when more
/// than three hunks fail.
#[rstest]
#[tokio::test]
async fn stale_patch_bounds_oversized_anchor_context_and_caps_failures(
    #[future] patch_workspace: Result<PatchWorkspace>,
) -> Result<()> {
    const EXCERPT_BUDGET: usize = 240;
    let PatchWorkspace { registry, _temp_dir: temp_dir } = patch_workspace.await?;
    let path = temp_dir.path().join("versioned.txt");
    let current_line = format!("current-{}", "c".repeat(EXCERPT_BUDGET + 40));
    fs::write(&path, format!("{current_line}\n"))?;

    let mut patch = String::from("*** Begin Patch\n*** Update File: versioned.txt\n");
    for index in 0..4 {
        let anchor = format!("anchor-{index}-{}", "a".repeat(EXCERPT_BUDGET + 40));
        let expected = format!("expected-{index}-{}", "e".repeat(EXCERPT_BUDGET + 40));
        patch.push_str(&format!("@@ {anchor}\n-{expected}\n+replacement-{index}\n"));
    }
    patch.push_str("*** End Patch\n");
    let expected_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(patch.as_bytes()));

    let outcome = registry
        .execute_public_tool_request(ToolExecutionRequest::new(
            tools::APPLY_PATCH,
            json!({ "input": patch, "expected_content_hash": expected_hash }),
        ))
        .await;

    let details = outcome.error.and_then(|error| error.details).expect("mismatch details");
    assert_eq!(details["can_safely_rebase"], false);
    let failures = details["anchor_failures"].as_array().expect("anchor failures");
    assert_eq!(failures.len(), 3, "failures must stay capped at three");
    for failure in failures {
        for field in ["anchor", "expected_excerpt", "current_excerpt"] {
            let excerpt = failure[field]
                .as_str()
                .unwrap_or_else(|| panic!("{field} must be a string, got {}", failure[field]));
            assert!(
                excerpt.chars().count() <= EXCERPT_BUDGET,
                "{field} must be bounded to {EXCERPT_BUDGET} characters, got {}",
                excerpt.chars().count()
            );
        }
    }
    assert_eq!(fs::read_to_string(path)?, format!("{current_line}\n"));
    Ok(())
}

/// Check a matching hash lets the write proceed.
#[rstest]
#[tokio::test]
async fn matching_apply_patch_hash_allows_the_write(#[future] patch_workspace: Result<PatchWorkspace>) -> Result<()> {
    let PatchWorkspace { registry, _temp_dir: temp_dir } = patch_workspace.await?;
    let path = temp_dir.path().join("versioned.txt");
    fs::write(&path, "current\n")?;
    let current = format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"current\n"));
    let patch = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n";

    let response = registry
        .execute_tool(tools::APPLY_PATCH, json!({ "input": patch, "expected_content_hash": current }))
        .await?;

    assert_eq!(response["success"], true);
    assert_eq!(fs::read_to_string(path)?, "changed\n");
    Ok(())
}

/// Check a matching hash is accepted for the single-source delete and rename forms.
#[rstest]
#[tokio::test]
async fn matching_apply_patch_hash_allows_single_source_delete_and_rename(
    #[future] patch_workspace: Result<PatchWorkspace>,
) -> Result<()> {
    let PatchWorkspace { registry, _temp_dir: temp_dir } = patch_workspace.await?;
    let obsolete = temp_dir.path().join("obsolete.txt");
    let renamed_from = temp_dir.path().join("before.txt");
    let renamed_to = temp_dir.path().join("after.txt");
    fs::write(&obsolete, "obsolete\n")?;
    fs::write(&renamed_from, "moved\n")?;

    let delete_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"obsolete\n"));
    let delete_patch = "*** Begin Patch\n*** Delete File: obsolete.txt\n*** End Patch\n";
    let delete_response = registry
        .execute_tool(tools::APPLY_PATCH, json!({ "input": delete_patch, "expected_content_hash": delete_hash }))
        .await?;
    assert_eq!(delete_response["success"], true);
    assert!(!obsolete.exists());

    let rename_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"moved\n"));
    let rename_patch =
        "*** Begin Patch\n*** Update File: before.txt\n*** Move to: after.txt\n@@\n-moved\n+relocated\n*** End Patch\n";
    let rename_response = registry
        .execute_tool(tools::APPLY_PATCH, json!({ "input": rename_patch, "expected_content_hash": rename_hash }))
        .await?;
    assert_eq!(rename_response["success"], true);
    assert!(!renamed_from.exists());
    assert_eq!(fs::read_to_string(renamed_to)?, "relocated\n");
    Ok(())
}
