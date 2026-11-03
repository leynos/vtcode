//! No-op-ladder coverage for repeated and reordered apply-patch updates.

use super::*;

/// Check identical no-ops escalate across registry clones without writing.
#[tokio::test]
async fn identical_no_op_patch_escalates_across_registry_clones_without_writing() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path().join("same.txt");
    fs::write(&path, "same\n")?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;
    let breaker = Arc::new(crate::tools::circuit_breaker::CircuitBreaker::new(Default::default()));
    registry.set_shared_circuit_breaker(Arc::clone(&breaker));
    let patch = "*** Begin Patch\n*** Update File: same.txt\n@@\n-same\n+same\n*** End Patch\n";
    #[cfg(unix)]
    let original_inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path)?.ino()
    };

    let first = registry.execute_tool(tools::APPLY_PATCH, json!({ "input": patch })).await?;
    assert_eq!(first["success"], true);
    assert_eq!(first["no_op"], true);
    assert_eq!(first["occurrence"], 1);
    assert!(first.get("modified_files").is_none());
    assert!(first["message"].as_str().is_some_and(|message| message.contains("final bytes")));

    let second_registry = registry.clone();
    let third_registry = registry.clone();
    let encoded_patch = format!("base64:{}", base64::engine::general_purpose::STANDARD.encode(patch.as_bytes()));
    let left = tokio::spawn(async move {
        second_registry
            .execute_public_tool_request(ToolExecutionRequest::new(tools::APPLY_PATCH, json!({ "patch": patch })))
            .await
    });
    let right = tokio::spawn(async move {
        third_registry
            .execute_public_tool_request(ToolExecutionRequest::new(
                tools::APPLY_PATCH,
                json!({ "input": encoded_patch }),
            ))
            .await
    });
    let (left, right) = tokio::try_join!(left, right)?;
    let (second, third) = if left.is_success() {
        (left, right)
    } else {
        (right, left)
    };
    let second = second.output.expect("second no-op response");
    assert_eq!(second["success"], true);
    assert_eq!(second["no_op"], true);
    assert_eq!(second["occurrence"], 2);
    assert_eq!(second["retry_prohibited"], true);
    assert!(second.get("modified_files").is_none());

    let error = third.error.expect("third identical no-op must be blocked");
    assert_eq!(error.error_type, ToolErrorType::PolicyViolation);
    assert!(!error.retryable);
    assert!(!error.circuit_breaker_impact);
    assert_eq!(
        error.details.as_ref().and_then(|details| details["reason"].as_str()),
        Some("repeated_identical_no_op")
    );
    assert_eq!(breaker.state_for_tool(tools::APPLY_PATCH), crate::tools::circuit_breaker::CircuitState::Closed);
    assert_eq!(fs::read_to_string(path)?, "same\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(temp_dir.path().join("same.txt"))?.ino(), original_inode);
    }

    let different = "*** Begin Patch\n*** Update File: same.txt\n@@\n-same\n+different\n*** End Patch\n";
    let available = registry.execute_tool(tools::APPLY_PATCH, json!({ "input": different })).await?;
    assert_eq!(available["success"], true);
    assert!(available.get("no_op").is_none());
    assert_eq!(fs::read_to_string(temp_dir.path().join("same.txt"))?, "different\n");
    Ok(())
}

/// Check repeated updates to one path apply in order instead of as a no-op.
#[tokio::test]
async fn repeated_updates_to_one_path_apply_in_order_instead_of_reporting_a_no_op() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path().join("chain.txt");
    fs::write(&path, "alpha\nbeta\n")?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;
    // Both operations target the same file. Planning every operation against the
    // pre-patch snapshot made the planned final state equal the original, so the
    // guard reported a no-op and the productive first update never ran.
    let patch = concat!(
        "*** Begin Patch\n",
        "*** Update File: chain.txt\n",
        "@@\n",
        "-alpha\n",
        "+ALPHA\n",
        "*** Update File: chain.txt\n",
        "@@\n",
        "-beta\n",
        "+beta\n",
        "*** End Patch\n",
    );

    let response = registry.execute_tool(tools::APPLY_PATCH, json!({ "input": patch })).await?;

    assert_eq!(response["success"], true);
    assert!(response.get("no_op").is_none(), "chained updates must not be reported as a no-op");
    assert_eq!(fs::read_to_string(&path)?, "ALPHA\nbeta\n");
    Ok(())
}

/// Check reordering independent no-op updates cannot reset the ladder.
#[tokio::test]
async fn reordered_independent_no_op_updates_escalate_the_ladder() -> Result<()> {
    let temp_dir = TempDir::new()?;
    fs::write(temp_dir.path().join("a.txt"), "alpha\n")?;
    fs::write(temp_dir.path().join("b.txt"), "beta\n")?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;
    let forward = concat!(
        "*** Begin Patch\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-alpha\n",
        "+alpha\n",
        "*** Update File: b.txt\n",
        "@@\n",
        "-beta\n",
        "+beta\n",
        "*** End Patch\n",
    );
    let reverse = concat!(
        "*** Begin Patch\n",
        "*** Update File: b.txt\n",
        "@@\n",
        "-beta\n",
        "+beta\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-alpha\n",
        "+alpha\n",
        "*** End Patch\n",
    );

    let first = registry.execute_tool(tools::APPLY_PATCH, json!({ "input": forward })).await?;
    assert_eq!(first["success"], true);
    assert_eq!(first["no_op"], true);
    assert_eq!(first["occurrence"], 1);

    let second = registry.execute_tool(tools::APPLY_PATCH, json!({ "input": reverse })).await?;
    assert_eq!(second["success"], true);
    assert_eq!(second["no_op"], true);
    assert_eq!(second["occurrence"], 2);
    assert_eq!(second["warning"], true);
    assert_eq!(second["retry_prohibited"], true);

    let error = registry
        .execute_public_tool_request(ToolExecutionRequest::new(tools::APPLY_PATCH, json!({ "input": forward })))
        .await
        .error
        .expect("third reordered no-op must be blocked");
    assert_eq!(error.error_type, ToolErrorType::PolicyViolation);
    assert!(!error.retryable);
    assert_eq!(
        error.details.as_ref().and_then(|details| details["reason"].as_str()),
        Some("repeated_identical_no_op")
    );
    assert_eq!(fs::read_to_string(temp_dir.path().join("a.txt"))?, "alpha\n");
    assert_eq!(fs::read_to_string(temp_dir.path().join("b.txt"))?, "beta\n");
    Ok(())
}

/// Check malformed hashes and invalid scopes are rejected.
#[tokio::test]
async fn apply_patch_hash_rejects_malformed_and_invalid_scopes() -> Result<()> {
    let temp_dir = TempDir::new()?;
    fs::write(temp_dir.path().join("one.txt"), "one\n")?;
    fs::write(temp_dir.path().join("two.txt"), "two\n")?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;
    let valid = format!("sha256:{}", "0".repeat(64));
    let cases = [
        (
            "sha256:ABC",
            "*** Begin Patch\n*** Update File: one.txt\n@@\n-one\n+changed\n*** End Patch\n",
            "invalid_expected_content_hash",
        ),
        (
            valid.as_str(),
            "*** Begin Patch\n*** Add File: added.txt\n+added\n*** End Patch\n",
            "invalid_precondition_scope",
        ),
        (
            valid.as_str(),
            "*** Begin Patch\n*** Delete File: one.txt\n*** Delete File: two.txt\n*** End Patch\n",
            "invalid_precondition_scope",
        ),
    ];

    for (hash, patch, reason) in cases {
        let outcome = registry
            .execute_public_tool_request(ToolExecutionRequest::new(
                tools::APPLY_PATCH,
                json!({ "input": patch, "expected_content_hash": hash }),
            ))
            .await;
        let error = outcome.error.expect("invalid precondition error");
        assert_eq!(error.error_type, ToolErrorType::InvalidParameters);
        assert_eq!(error.details.as_ref().and_then(|details| details["reason"].as_str()), Some(reason));
    }

    assert_eq!(fs::read_to_string(temp_dir.path().join("one.txt"))?, "one\n");
    assert_eq!(fs::read_to_string(temp_dir.path().join("two.txt"))?, "two\n");
    assert!(!temp_dir.path().join("added.txt").exists());
    Ok(())
}
