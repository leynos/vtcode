//! Opposing attachments: the guard makes the cycle check and publication
//! one critical section.

use super::*;

use crate::subagents::controller_tree::attachment_checkpoints::AttachmentStage;

/// Two attachments in opposite directions cannot both pass the cycle check.
///
/// The disjointness checks and the publication must be one critical section:
/// if each attachment can observe the other tree before either publishes, both
/// accept and the two links form an `Arc` cycle. The test parks the first
/// attachment on the session lock it resolves its observer key through, which
/// leaves it holding only state it checked, then lets the opposite attachment
/// run to completion. The first must re-check under the tree's attachment guard
/// and reject the edge that is now cyclic.
///
/// Against the unguarded code this test fails deterministically: the first
/// attachment resumes with its pre-race result and publishes the second edge.
#[tokio::test]
async fn opposing_concurrent_attachments_publish_exactly_once() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    // Both controllers share one tree scope, so the child-id adoption would
    // accept the graft; only the cycle check can reject it.
    let scope = Arc::new(SubagentLockScope::new());
    let mut left_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
    left_config.lock_scope = Some(Arc::clone(&scope));
    let left = Arc::new(SubagentController::new(left_config).await.expect("left controller"));
    let mut right_config = test_controller_config(workspace_root, DiapasonConfig::default());
    right_config.lock_scope = Some(Arc::clone(&scope));
    let right = Arc::new(SubagentController::new(right_config).await.expect("right controller"));
    let spec = explorer_spec();

    // Each tree tracks an unattached record for the other. An empty
    // `parent_thread_id` makes `observer_scope_for` await the controller's
    // session lock, which is where the first attachment is parked below.
    left.state.write().await.children.insert(
        "right".to_string(),
        test_child_record("right", "right-session", "", &spec, SubagentStatus::Running, 1, None),
    );
    right.state.write().await.children.insert(
        "left".to_string(),
        test_child_record("left", "left-session", "", &spec, SubagentStatus::Running, 1, None),
    );

    let session_guard = left.parent_session_id.write().await;
    let first_future = left.attach_child_controller("right", Arc::clone(&right));
    let mut first_future = std::pin::pin!(first_future);
    // Poll the first attachment until it parks on the held session lock. On the
    // unguarded code that is after both disjointness checks; on the guarded
    // code it is before them, so the guard is what makes it re-check.
    tokio::select! {
        biased;
        outcome = &mut first_future => {
            panic!("the first attachment must park on the held session lock: {outcome:?}");
        }
        _ = tokio::task::yield_now() => {}
    }

    // The opposite attachment runs while the first is parked. It publishes
    // right -> left, so left -> right would close the cycle. Bound it: a
    // broken lock order would otherwise wait on the session lock this test
    // still holds and hang instead of failing.
    let opposite =
        tokio::time::timeout(Duration::from_secs(5), right.attach_child_controller("left", Arc::clone(&left))).await;
    let opposite = match opposite {
        Ok(outcome) => outcome,
        Err(_) => {
            drop(session_guard);
            panic!(
                "the opposite attachment must publish while the first is parked on the session lock; \
                 it timed out, so the attachment path waits on the held lock instead"
            );
        }
    };
    opposite.expect("the first of the two opposing attachments must publish");

    drop(session_guard);
    let rejection = first_future
        .await
        .expect_err("the second of two opposing attachments must be rejected");
    assert!(
        format!("{rejection}").contains("already part of this tree"),
        "the rejection must name the cycle: {rejection}"
    );

    // Exactly one edge is published, and the rejected record stays unattached,
    // so a traversal cannot loop.
    assert!(
        left.state
            .read()
            .await
            .children
            .get("right")
            .expect("the tracked record survives rejection")
            .child_controller
            .is_none(),
        "a rejected attachment must not publish the opposite edge"
    );
    assert!(
        right
            .state
            .read()
            .await
            .children
            .get("left")
            .expect("the accepted record is published")
            .child_controller
            .as_ref()
            .is_some_and(|attached| Arc::ptr_eq(&attached.state, &left.state)),
        "the accepted attachment must stay published"
    );
    assert!(
        right
            .descendant_controllers()
            .await
            .iter()
            .any(|controller| Arc::ptr_eq(controller, &left)),
        "the accepted attachment must be reachable from the tree"
    );

    right.signal_shutdown().await;
    left.signal_shutdown().await;
}

/// The attachment guard, not an earlier check, keeps opposing edges apart.
///
/// The existing opposing test parks the first attachment before its cycle
/// check. This one parks it after the check and while the guard is held, so the
/// second attachment has already passed every pre-guard test and can only be
/// stopped by the guard itself. Without the guard both attachments pass their
/// checks and publish, forming a cycle.
///
/// The scenario runs at both checkpoints. Parking immediately after the cycle
/// check proves the guard covers the checks; parking immediately before the
/// published link proves it is still held through the remaining awaits — the
/// subtree walk, the state lock and the allocator adoption.
#[tokio::test]
async fn an_attachment_holds_the_guard_from_its_cycle_check_to_publication() {
    for (stage, checkpoint) in [
        (AttachmentStage::AfterCycleCheck, "after its cycle check"),
        (AttachmentStage::BeforePublication, "immediately before the link is published"),
    ] {
        // The scenario carries two controller trees and their controllers, so
        // its future is far too large to keep inline in this loop.
        Box::pin(assert_attachment_guard_spans_the_stage(stage, checkpoint)).await;
    }
}

/// Runs the guard-span scenario with the first attachment parked at `stage`.
async fn assert_attachment_guard_spans_the_stage(stage: AttachmentStage, checkpoint: &str) {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let scope = Arc::new(SubagentLockScope::new());
    let mut left_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
    left_config.lock_scope = Some(Arc::clone(&scope));
    let left = Arc::new(SubagentController::new(left_config).await.expect("left controller"));
    let mut right_config = test_controller_config(workspace_root, DiapasonConfig::default());
    right_config.lock_scope = Some(Arc::clone(&scope));
    let right = Arc::new(SubagentController::new(right_config).await.expect("right controller"));
    let spec = explorer_spec();

    left.state.write().await.children.insert(
        "right".to_string(),
        test_child_record("right", "right-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    right.state.write().await.children.insert(
        "left".to_string(),
        test_child_record("left", "left-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );

    let (entered, release) = controller_tree::attachment_checkpoints::arm(&left.state, stage);
    let first = tokio::spawn({
        let left = Arc::clone(&left);
        let right = Arc::clone(&right);
        async move { left.attach_child_controller("right", right).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap_or_else(|_| panic!("the first attachment must reach the checkpoint {checkpoint}"));

    // At `BeforePublication` the parked attachment already holds the state
    // lock, so the opposite attachment would be blocked by that lock whether or
    // not the attachment guard is still held. The observable block therefore
    // proves nothing about the guard at this stage. Read the guard directly:
    // it must be held now, and it must be free once the attach completes.
    if stage == AttachmentStage::BeforePublication {
        assert!(
            left.lock_scope().attachment().try_lock().is_err(),
            "the attachment guard must still be held at the point the link is published"
        );
    }

    // The opposite attachment has passed no guard yet; it must block on the
    // guard the parked attachment still holds rather than publish.
    let second =
        tokio::time::timeout(Duration::from_millis(100), right.attach_child_controller("left", Arc::clone(&left)))
            .await;
    assert!(second.is_err(), "the second attachment must wait for the guard the first still holds");

    release.notify_one();
    first
        .await
        .expect("the first attachment task must not panic")
        .expect("the first attachment publishes");
    if stage == AttachmentStage::BeforePublication {
        // The guard was held at the publication point and is released with the
        // attachment: a guard that stayed held would serialise every later
        // attachment in the tree forever.
        assert!(
            left.lock_scope().attachment().try_lock().is_ok(),
            "the attachment guard must be released once the link is published"
        );
    }
    let rejection = right
        .attach_child_controller("left", Arc::clone(&left))
        .await
        .expect_err("the second attachment must reject the cycle once the guard is released");
    assert!(
        format!("{rejection}").contains("already part of this tree"),
        "the rejection must name the cycle: {rejection}"
    );

    assert!(
        left.state
            .read()
            .await
            .children
            .get("right")
            .expect("the accepted record is published")
            .child_controller
            .as_ref()
            .is_some_and(|attached| Arc::ptr_eq(&attached.state, &right.state)),
        "exactly the first edge must be published"
    );
    assert!(
        right
            .state
            .read()
            .await
            .children
            .get("left")
            .expect("the rejected record survives")
            .child_controller
            .is_none(),
        "the rejected attachment must not publish the opposite edge"
    );

    right.signal_shutdown().await;
    left.signal_shutdown().await;
}
