//! Attachment guards: cycle rejection and record ownership.
//!
//! An attachment mutates the tree, so a request that would close a cycle or
//! displace a controller whose tasks would become unreachable must be refused
//! before the incoming allocator is adopted.
use super::*;

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
    // right -> left, so left -> right would close the cycle.
    right
        .attach_child_controller("left", Arc::clone(&left))
        .await
        .expect("the first of the two opposing attachments must publish");

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

/// Every attachment path shares the occupied-record rule.
///
/// `record_child_runtime` calls the shared helper after `begin_child_run`
/// snapshotted the record's controller. Without the rule in the helper, a
/// controller attached in that window is silently replaced and its own
/// subtree drops out of the tree. Re-publishing the controller the record
/// already names stays idempotent.
#[tokio::test]
async fn the_shared_state_helper_refuses_to_replace_a_published_controller() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = Arc::new(
        SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
            .await
            .expect("root controller"),
    );
    let child = nested_child_controller(workspace_root.clone(), "child-session").await;
    let replacement = nested_child_controller(workspace_root, "replacement-session").await;
    let spec = explorer_spec();

    root.state.write().await.children.insert(
        "child".to_string(),
        test_child_record("child", "child-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    root.attach_child_controller("child", Arc::clone(&child))
        .await
        .expect("the nested controller joins the root");

    let mut state = root.state.write().await;
    let rejection = SubagentController::attach_child_controller_to_state(&mut state, "child", Arc::clone(&replacement))
        .expect_err("the shared helper must refuse to displace a published controller");
    assert!(
        format!("{rejection}").contains("already attached to another controller"),
        "the rejection must name the occupied record: {rejection}"
    );
    SubagentController::attach_child_controller_to_state(&mut state, "child", Arc::clone(&child))
        .expect("re-publishing the controller the record names is idempotent");
    drop(state);

    // The original controller and its subtree stay reachable; the rejected
    // replacement is never published.
    let descendants = root.descendant_controllers().await;
    assert!(
        descendants.iter().any(|controller| Arc::ptr_eq(controller, &child)),
        "the tracked controller must stay reachable"
    );
    assert!(
        !descendants.iter().any(|controller| Arc::ptr_eq(controller, &replacement)),
        "the rejected controller must not be published"
    );

    child.signal_shutdown().await;
    replacement.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// An attachment that would close a cycle is rejected, while an ordinary
/// nested attachment still joins the tree.
///
/// The tree is walked parent to child, so a link back to an ancestor (or to the
/// controller itself) would make traversal revisit a controller it already
/// reached. The record must also stay unattached after the rejection, because
/// the incoming allocator is adopted only after the check.
#[tokio::test]
async fn cyclic_attachments_are_rejected_without_disrupting_the_tree() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = Arc::new(
        SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
            .await
            .expect("root controller"),
    );
    let child = nested_child_controller(workspace_root, "child-session").await;
    let spec = explorer_spec();

    // A legitimate nested attachment still succeeds.
    root.state.write().await.children.insert(
        "child".to_string(),
        test_child_record("child", "child-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    root.attach_child_controller("child", Arc::clone(&child))
        .await
        .expect("a nested attachment must still join the tree");
    assert_eq!(root.descendant_controllers().await.len(), 1, "the legitimate attachment must be published");

    // A link back to the root would close root -> child -> root.
    child.state.write().await.children.insert(
        "root".to_string(),
        test_child_record("root", "parent-session", "child-session", &spec, SubagentStatus::Running, 2, None),
    );
    let cycle = child
        .attach_child_controller("root", Arc::clone(&root))
        .await
        .expect_err("an ancestor cannot be attached beneath its own descendant");
    assert!(
        format!("{cycle}").contains("already part of this tree"),
        "the rejection must name the cycle: {cycle}"
    );
    assert!(
        child
            .state
            .read()
            .await
            .children
            .get("root")
            .expect("the tracked record survives rejection")
            .child_controller
            .is_none(),
        "a rejected cyclic attachment must not publish the controller"
    );

    // Attaching a controller to itself is the same closed loop.
    root.state.write().await.children.insert(
        "self".to_string(),
        test_child_record("self", "parent-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    let self_cycle = root
        .attach_child_controller("self", Arc::clone(&root))
        .await
        .expect_err("a controller cannot be attached to itself");
    assert!(
        format!("{self_cycle}").contains("already part of this tree"),
        "the rejection must name the cycle: {self_cycle}"
    );
    assert!(
        root.state
            .read()
            .await
            .children
            .get("self")
            .expect("the tracked record survives rejection")
            .child_controller
            .is_none(),
        "a rejected self attachment must not publish the controller"
    );

    child.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// A record that already tracks a controller cannot be re-pointed at another.
///
/// The displaced controller keeps the children it tracks. Replacing it would
/// drop those tasks out of the tree's reachable set, so a running grandchild
/// would be stranded: management and output routing walk the published tree and
/// would no longer find it. Re-attaching the controller a record already names
/// stays valid.
#[tokio::test]
async fn attaching_over_an_occupied_record_keeps_tracked_work_reachable() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = Arc::new(
        SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
            .await
            .expect("root controller"),
    );
    let child = nested_child_controller(workspace_root.clone(), "child-session").await;
    let grandchild = nested_child_controller(workspace_root.clone(), "grandchild-session").await;
    let replacement = nested_child_controller(workspace_root, "replacement-session").await;
    let spec = explorer_spec();

    // The child tracks a running grandchild of its own.
    child.state.write().await.children.insert(
        "grandchild".to_string(),
        test_child_record("grandchild", "grandchild-session", "child-session", &spec, SubagentStatus::Running, 2, None),
    );
    child
        .attach_child_controller("grandchild", Arc::clone(&grandchild))
        .await
        .expect("the grandchild joins the child");

    root.state.write().await.children.insert(
        "child".to_string(),
        test_child_record("child", "child-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    root.attach_child_controller("child", Arc::clone(&child))
        .await
        .expect("the nested controller joins the root");

    let error = root
        .attach_child_controller("child", Arc::clone(&replacement))
        .await
        .expect_err("a different controller cannot displace the one the record tracks");
    assert!(
        format!("{error}").contains("already attached to another controller"),
        "the rejection must name the occupied record: {error}"
    );

    // The original controller stays published and the grandchild stays
    // reachable through it.
    let descendants = root.descendant_controllers().await;
    assert!(
        descendants.iter().any(|controller| Arc::ptr_eq(controller, &child)),
        "the tracked child controller must stay reachable"
    );
    assert!(
        descendants.iter().any(|controller| Arc::ptr_eq(controller, &grandchild)),
        "the grandchild must stay reachable through the tracked child"
    );
    assert!(
        !descendants.iter().any(|controller| Arc::ptr_eq(controller, &replacement)),
        "the rejected controller must not be published"
    );
    assert!(
        root.state
            .read()
            .await
            .children
            .get("child")
            .expect("the tracked record survives rejection")
            .child_controller
            .as_ref()
            .is_some_and(|attached| Arc::ptr_eq(&attached.state, &child.state)),
        "the record must still name the controller it tracked"
    );

    // The flattened listing is what the adapter serves; the grandchild must
    // still appear exactly once.
    let mut listed = root
        .status_entries()
        .await
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    for controller in &descendants {
        listed.extend(controller.status_entries().await.into_iter().map(|entry| entry.id));
    }
    assert_eq!(
        listed.iter().filter(|id| id.as_str() == "grandchild").count(),
        1,
        "the grandchild must still be listed exactly once: {listed:?}"
    );

    // Re-attaching the same controller is idempotent, not a replacement.
    root.attach_child_controller("child", Arc::clone(&child))
        .await
        .expect("re-attaching the tracked controller must stay valid");

    for controller in [&child, &grandchild, &replacement] {
        controller.signal_shutdown().await;
    }
    root.signal_shutdown().await;
}
