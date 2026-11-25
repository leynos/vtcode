//! Attachment guards: cycle rejection and record ownership.
//!
//! An attachment mutates the tree, so a request that would close a cycle or
//! displace a controller whose tasks would become unreachable must be refused
//! before the incoming allocator is adopted.
use super::*;

mod opposing_attachments;

/// Records the child snapshots a controller publishes through one observer key.
#[derive(Default)]
struct RecordingObserver {
    children: Mutex<Vec<SubagentStatusEntry>>,
}

impl SubagentProgressObserver for RecordingObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.children.lock().expect("children lock").push(entry);
    }

    fn background_status_changed(&self, _entry: BackgroundSubprocessEntry) {}
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
    // The rejection must leave the published link untouched. Re-attaching the
    // same controller below is idempotent, so it would silently repair a helper
    // that cleared the record on its way out.
    let published = state
        .children
        .get("child")
        .expect("the tracked record survives the rejection")
        .child_controller
        .as_ref()
        .expect("a rejected replacement must not clear the published controller");
    assert!(Arc::ptr_eq(published, &child), "the rejected replacement must leave the original controller linked");
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

/// A controller already published into one tree cannot be grafted into a
/// second.
///
/// Two roots that share one lock scope have no conflicting task ids and no
/// reachable overlap, so the downward disjointness check passes for both. The
/// second graft would then rebind the controller's observer route to its own
/// tree while the first root kept a link whose controller no longer publishes
/// through it, so the first root would still list the subtree without ever
/// receiving its lifecycle updates. The owner is recorded when the link is
/// published, and the second graft is refused before the allocator or the route
/// is adopted. Re-attaching to the record that first listed the controller
/// stays idempotent, and the rejection leaves the first link and route intact.
#[tokio::test]
async fn a_controller_already_published_in_one_tree_cannot_be_grafted_into_another() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let first_root = Arc::new(
        SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
            .await
            .expect("first root"),
    );
    let second_root = Arc::new(
        SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
            .await
            .expect("second root"),
    );
    let shared = nested_child_controller(workspace_root, "shared-session").await;
    let spec = explorer_spec();

    for (root, id, parent) in [
        (Arc::clone(&first_root), "shared", "parent-session"),
        (Arc::clone(&second_root), "shared", "parent-session"),
    ] {
        root.state.write().await.children.insert(
            id.to_string(),
            test_child_record(id, "shared-session", parent, &spec, SubagentStatus::Running, 1, None),
        );
    }

    first_root
        .attach_child_controller("shared", Arc::clone(&shared))
        .await
        .expect("the controller joins the first tree");

    // The observer registered on the first root must keep receiving the
    // controller's snapshots after the second graft is refused.
    let first_observer = Arc::new(RecordingObserver::default());
    let _first_registration = first_root.register_progress_observer("parent-session", first_observer.clone());

    let rejection = second_root
        .attach_child_controller("shared", Arc::clone(&shared))
        .await
        .expect_err("a controller owned by another tree cannot be grafted again");
    assert!(
        format!("{rejection}").contains("already belongs to another tree"),
        "the rejection must name the ownership: {rejection}"
    );

    // The first link survives the rejection, and the second root publishes no
    // link at all.
    assert!(
        first_root
            .state
            .read()
            .await
            .children
            .get("shared")
            .expect("the first record survives the rejection")
            .child_controller
            .as_ref()
            .is_some_and(|attached| Arc::ptr_eq(attached, &shared)),
        "the rejection must leave the first tree's link unchanged"
    );
    assert!(
        second_root
            .state
            .read()
            .await
            .children
            .get("shared")
            .expect("the second record survives the rejection")
            .child_controller
            .is_none(),
        "a rejected graft must not publish the controller"
    );
    assert!(
        second_root.descendant_controllers().await.is_empty(),
        "a rejected graft must not make the controller reachable from the second tree"
    );

    // The route still points at the first tree: a snapshot the controller
    // publishes reaches the first root's observer.
    shared.state.write().await.children.insert(
        "grandchild".to_string(),
        test_child_record(
            "grandchild",
            "grandchild-session",
            "shared-session",
            &spec,
            SubagentStatus::Running,
            2,
            None,
        ),
    );
    shared.publish_child_status("grandchild").await;
    {
        let updates = first_observer.children.lock().expect("children lock");
        assert_eq!(
            updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            vec!["grandchild"],
            "a refused second graft must leave the first tree's route in place"
        );
    }

    // Re-attaching to the record that first listed the controller is
    // idempotent rather than an ownership violation.
    first_root
        .attach_child_controller("shared", Arc::clone(&shared))
        .await
        .expect("re-attaching the controller to its own tree must stay valid");

    shared.signal_shutdown().await;
    first_root.signal_shutdown().await;
    second_root.signal_shutdown().await;
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
