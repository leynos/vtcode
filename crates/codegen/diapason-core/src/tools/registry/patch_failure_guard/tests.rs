//! Unit coverage for the failing-`apply_patch` repeat bound.

use super::*;
use serde_json::json;

/// Two identical failures are permitted and the third call is refused.
#[test]
fn identical_failures_block_the_third_attempt() {
    let mut guard = PatchFailureGuard::default();
    let signature = request_signature(&json!({ "input": "patch" }));

    assert_eq!(guard.blocked_occurrence(&signature), None, "no attempt has failed yet");

    guard.record(&signature, true);
    assert_eq!(guard.blocked_occurrence(&signature), None, "the first failure is not a repeat");

    guard.record(&signature, true);
    assert_eq!(guard.blocked_occurrence(&signature), Some(3), "the third identical call is refused");
    assert_eq!(guard.blocked_occurrence(&signature), Some(3), "a refused call does not advance the ladder");

    guard.record(&signature, true);
    assert_eq!(guard.blocked_occurrence(&signature), Some(4), "the ladder keeps escalating while repeats continue");
}

/// A materially different request, or a success, clears the ladder.
#[test]
fn a_different_request_or_a_success_restarts_the_ladder() {
    let mut guard = PatchFailureGuard::default();
    let first = request_signature(&json!({ "input": "first" }));
    let second = request_signature(&json!({ "input": "second" }));

    guard.record(&first, true);
    guard.record(&first, true);
    assert_eq!(guard.blocked_occurrence(&first), Some(3));

    guard.record(&second, true);
    assert_eq!(guard.blocked_occurrence(&first), None, "a different request starts a new ladder");
    assert_eq!(guard.blocked_occurrence(&second), None, "one failure of the new request is not a repeat");

    guard.record(&second, true);
    assert_eq!(guard.blocked_occurrence(&second), Some(3));

    guard.record(&second, false);
    assert_eq!(guard.blocked_occurrence(&second), None, "a success clears the ladder");
    assert_eq!(guard.blocked_occurrence(&first), None);
}

/// Equivalent key orderings describe the same request; different payloads do not.
#[test]
fn the_signature_ignores_key_order_and_separates_distinct_payloads() {
    let forward = json!({ "input": "*** Begin Patch", "expected_content_hash": "sha256:00" });
    let reverse = json!({ "expected_content_hash": "sha256:00", "input": "*** Begin Patch" });
    assert_eq!(request_signature(&forward), request_signature(&reverse));

    let other_hash = json!({ "input": "*** Begin Patch", "expected_content_hash": "sha256:11" });
    assert_ne!(request_signature(&forward), request_signature(&other_hash));

    let aliased = json!({ "a": "bc" });
    let split = json!({ "ab": "c" });
    assert_ne!(
        request_signature(&aliased),
        request_signature(&split),
        "length prefixes must stop adjacent fields aliasing"
    );
}

/// Structured rejections, conflicts, and propagated errors all count as failures.
#[test]
fn failure_classification_covers_every_rejection_shape() {
    let rejected = Ok(json!({ "success": false, "error": { "message": "content_hash_mismatch" } }));
    assert!(attempt_failed(&rejected), "an error envelope is a failure");

    let conflict = Ok(json!({ "success": true, "conflict_detected": true }));
    assert!(attempt_failed(&conflict), "a conflict is a failure even though it reports success");

    let applied = Ok(json!({ "success": true, "applied": [] }));
    assert!(!attempt_failed(&applied), "a productive patch is not a failure");

    let no_op = Ok(json!({ "success": true, "no_op": true, "occurrence": 1 }));
    assert!(!attempt_failed(&no_op), "the no-op ladder owns its own repeat bound");

    let propagated: anyhow::Result<Value> = Err(anyhow::anyhow!("patch parse failed"));
    assert!(attempt_failed(&propagated), "a propagated error is a failure");
}
