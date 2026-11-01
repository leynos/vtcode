//! `expected_content_hash` behaviour on the `ApplyPatchHandler` surface.
//!
//! These drive the real `ApplyPatchHandler::handle` entry point, so payload
//! decoding, the orchestrator, and the runtime recheck are all exercised
//! together rather than the precondition helper alone.

use super::*;
use rstest::rstest;
use serde_json::json;

/// Check a stale expected hash is rejected before the handler writes.
#[tokio::test]
async fn json_handler_enforces_expected_content_hash_before_apply() {
    let workspace = TempDir::new().expect("temporary workspace");
    std::fs::write(workspace.path().join("versioned.txt"), "current\n").expect("write fixture");
    let patch =
        Patch::parse("*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n")
            .expect("valid patch");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));

    let error = validate_request_content_hash(Some(&stale_hash), &patch, workspace.path(), workspace.path())
        .await
        .expect_err("stale handler precondition");

    let message = error.to_string();
    assert!(message.contains("content_hash_mismatch"));
    assert!(message.contains("current_content_hash"));
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("versioned.txt")).expect("read unchanged fixture"),
        "current\n"
    );
}

/// The JSON `expected_content_hash` precondition must be enforced by the
/// handler *runtime*, not only by the precondition helper. This drives the
/// real `ApplyPatchHandler::handle` entry point so the payload decoding,
/// orchestrator, and runtime wiring are all covered.
#[tokio::test]
async fn handler_runtime_rejects_stale_expected_content_hash() {
    let workspace = TempDir::new().expect("temporary workspace");
    let fixture = workspace.path().join("versioned.txt");
    std::fs::write(&fixture, "current\n").expect("write fixture");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));
    let arguments = json!({
        "input": "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n",
        "expected_content_hash": stale_hash,
    })
    .to_string();

    let error = ApplyPatchHandler
        .handle(test_invocation(workspace.path().to_path_buf(), arguments))
        .await
        .expect_err("the handler must reject a stale expected_content_hash");

    let message = error.to_string();
    assert!(message.contains("content_hash_mismatch"), "unexpected error: {message}");
    assert!(message.contains("current_content_hash"), "unexpected error: {message}");
    assert_eq!(
        std::fs::read_to_string(&fixture).expect("read unchanged fixture"),
        "current\n",
        "a rejected precondition must not mutate the file"
    );
}

/// A matching `expected_content_hash` must let the handler through to the
/// mutation itself, proving the guard is a precondition and not a blanket
/// rejection of versioned patches.
#[tokio::test]
async fn handler_runtime_applies_patch_with_matching_expected_content_hash() {
    let workspace = TempDir::new().expect("temporary workspace");
    let fixture = workspace.path().join("versioned.txt");
    std::fs::write(&fixture, "current\n").expect("write fixture");
    let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"current\n"));
    let arguments = json!({
        "input": "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n",
        "expected_content_hash": current_hash,
    })
    .to_string();

    ApplyPatchHandler
        .handle(test_invocation(workspace.path().to_path_buf(), arguments))
        .await
        .expect("a matching content hash must allow the patch to apply");

    assert_eq!(
        std::fs::read_to_string(&fixture).expect("read patched fixture"),
        "changed\n",
        "the handler should have applied the patch"
    );
}

/// An `expected_content_hash` can only gate a patch with exactly one
/// pre-existing source file, so the handler must reject an add-only or a
/// multi-source patch with the registry's scope envelope rather than a generic
/// message. Each row pins one reason the scope is invalid, so the two cases
/// stay distinguishable even though they share one disposition.
#[rstest]
#[case::add_only(&[], "*** Begin Patch\n*** Add File: fresh.txt\n+hello\n*** End Patch\n", 0, "add-only")]
#[case::multi_source(
    &["left.txt", "right.txt"],
    "*** Begin Patch\n*** Update File: left.txt\n@@\n-left.txt\n+changed left\n*** Update File: right.txt\n@@\n-right.txt\n+changed right\n*** End Patch\n",
    2,
    "exactly one pre-existing source file"
)]
#[tokio::test]
async fn handler_runtime_rejects_out_of_scope_expected_hash(
    #[case] fixtures: &[&str],
    #[case] patch: &str,
    #[case] source_file_count: usize,
    #[case] message_fragment: &str,
) {
    let workspace = TempDir::new().expect("temporary workspace");
    for name in fixtures {
        std::fs::write(workspace.path().join(name), format!("{name}\n")).expect("write fixture");
    }
    let expected_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"anything\n"));
    let arguments = json!({ "input": patch, "expected_content_hash": expected_hash }).to_string();

    let error = ApplyPatchHandler
        .handle(test_invocation(workspace.path().to_path_buf(), arguments))
        .await
        .expect_err("an out-of-scope expected_content_hash must be rejected");

    let envelope = precondition_envelope(&error);
    let body = &envelope["error"];
    assert_eq!(body["error_type"], "InvalidParameters", "unexpected error: {envelope}");
    assert_eq!(body["details"]["reason"], "invalid_precondition_scope", "unexpected error: {envelope}");
    assert_eq!(body["details"]["source_file_count"], source_file_count, "unexpected error: {envelope}");
    assert!(
        body["message"]
            .as_str()
            .expect("message should be a string")
            .contains(message_fragment),
        "unexpected error: {envelope}"
    );
    for name in fixtures {
        assert_eq!(
            std::fs::read_to_string(workspace.path().join(name)).expect("read unchanged fixture"),
            format!("{name}\n"),
            "a rejected precondition must not mutate the file"
        );
    }
    // The add-only row names a file the patch would have created; no row may
    // leave a new file behind.
    assert!(!workspace.path().join("fresh.txt").exists(), "a rejected precondition must not add files");
}

/// A non-UTF-8 file cannot be assessed for rebase, so the mismatch must
/// report the registry's binary shape: no safe rebase and no anchor
/// failures, rather than a UTF-8 decode error.
#[tokio::test]
async fn handler_runtime_reports_binary_mismatch_shape() {
    let workspace = TempDir::new().expect("temporary workspace");
    let fixture = workspace.path().join("binary.bin");
    let bytes = [0xff_u8, 0xfe, 0xfd];
    std::fs::write(&fixture, bytes).expect("write binary fixture");
    let expected_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"expected text\n"));
    let arguments = json!({
        "input": "*** Begin Patch\n*** Update File: binary.bin\n@@\n-old\n+new\n*** End Patch\n",
        "expected_content_hash": expected_hash,
    })
    .to_string();

    let error = ApplyPatchHandler
        .handle(test_invocation(workspace.path().to_path_buf(), arguments))
        .await
        .expect_err("a binary mismatch must be rejected");

    let envelope = precondition_envelope(&error);
    let details = &envelope["error"]["details"];
    assert_eq!(details["reason"], "content_hash_mismatch", "unexpected error: {envelope}");
    assert_eq!(details["can_safely_rebase"], false, "unexpected error: {envelope}");
    assert_eq!(details["anchor_failures"], json!([]), "unexpected error: {envelope}");
    assert_eq!(
        details["current_content_hash"],
        format!("sha256:{}", vtcode_commons::utils::calculate_sha256(&bytes)),
        "unexpected error: {envelope}"
    );
    assert_eq!(
        std::fs::read(&fixture).expect("read unchanged fixture"),
        bytes,
        "a rejected precondition must not mutate the file"
    );
}

/// A source file removed after the handler captured its version must report
/// a null current hash instead of the malformed empty `sha256:` value.
#[tokio::test]
async fn deleted_source_file_reports_a_null_current_hash() {
    let workspace = TempDir::new().expect("temporary workspace");
    let fixture = workspace.path().join("gone.txt");
    std::fs::write(&fixture, "captured\n").expect("write fixture");
    let expected_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"captured\n"));
    std::fs::remove_file(&fixture).expect("remove fixture");
    let patch = Patch::parse("*** Begin Patch\n*** Update File: gone.txt\n@@\n-captured\n+changed\n*** End Patch\n")
        .expect("valid patch");

    let error = validate_request_content_hash(Some(&expected_hash), &patch, workspace.path(), workspace.path())
        .await
        .expect_err("a removed source file must be rejected")
        .into_tool_call_error();

    let envelope = precondition_envelope(&error);
    let details = &envelope["error"]["details"];
    assert_eq!(details["reason"], "content_hash_mismatch", "unexpected error: {envelope}");
    assert!(details["current_content_hash"].is_null(), "a removed file has no version: {envelope}");
    assert!(
        !envelope.to_string().contains("\"sha256:\""),
        "the malformed empty hash must not appear: {envelope}"
    );
    assert!(!fixture.exists(), "a rejected precondition must not recreate the file");
}
