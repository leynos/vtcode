//! Graft rejection: a controller bound to another adoption lock cannot join.
use super::*;

/// A graft refuses a controller that is bound to another adoption lock.
///
/// The graft holds the tree's adoption lock so no id can be reserved while the
/// two allocators are reconciled. A controller bound to a different lock
/// cannot be made exclusive that way: an allocation in the tree could run
/// between the merge and the redirect. The attachment therefore fails closed,
/// and the rejection is atomic like the duplicate-id case.
///
/// The joining subtree is seeded with a reservation before the graft is
/// requested, so the atomicity asserted here is the real one: the reservoir
/// exists, and both allocators must still disagree about it afterwards.
#[tokio::test]
async fn attaching_a_controller_with_another_lock_scope_is_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let mut root_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
    root_config.lock_scope = Some(Arc::new(SubagentLockScope::new()));
    let root = Arc::new(SubagentController::new(root_config).await.expect("root controller"));
    let mut foreign_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
    foreign_config.lock_scope = Some(Arc::new(SubagentLockScope::new()));
    let foreign = Arc::new(SubagentController::new(foreign_config).await.expect("foreign controller"));
    let spec = explorer_spec();

    assert!(
        !Arc::ptr_eq(root.lock_scope(), foreign.lock_scope()),
        "the fixture must give the two controllers distinct adoption locks"
    );

    root.state.write().await.children.insert(
        "foreign".to_string(),
        test_child_record("foreign", "foreign-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );

    // Give the joining subtree a reservation the graft would have to carry
    // over. Rejecting an empty allocator proves nothing: a graft that merged
    // first and rejected afterwards would have nothing to lose. With a
    // reservation in place, the rejection must leave it in the foreign
    // allocator, and the root must still be free to issue the same id.
    let seeded = "agent-explorer-20261118T041400000Z";
    let foreign_child = foreign
        .insert_child_with_candidate_id(
            seeded,
            test_child_record(seeded, "foreign-seeded", "foreign-session", &spec, SubagentStatus::Queued, 2, None),
        )
        .await
        .expect("seed a reservation in the joining subtree");
    assert_eq!(foreign_child, seeded, "the seeded reservation must be issued verbatim");

    let error = root
        .attach_child_controller("foreign", Arc::clone(&foreign))
        .await
        .expect_err("a controller bound to another adoption lock cannot be grafted");
    assert!(format!("{error}").contains("lock scope"), "the rejection must name the scope mismatch: {error}");

    // The seeded reservation stayed in the foreign allocator, so the root can
    // still issue the same id rather than suffixing it.
    let root_seeded = root
        .insert_child_with_candidate_id(
            seeded,
            test_child_record(seeded, "root-seeded", "parent-session", &spec, SubagentStatus::Queued, 1, None),
        )
        .await
        .expect("insert into the root after the rejected attachment");
    assert_eq!(root_seeded, seeded, "a rejected graft must not have moved the reservation into the root allocator");

    assert!(
        root.state
            .read()
            .await
            .children
            .get("foreign")
            .expect("the tracked record survives rejection")
            .child_controller
            .is_none(),
        "a rejected attachment must not publish the controller"
    );
    assert!(
        root.descendant_controllers().await.is_empty(),
        "a rejected attachment must not make the foreign subtree reachable"
    );

    let candidate = "agent-explorer-20261118T041500000Z";
    let foreign_id = foreign
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "foreign-candidate",
                "foreign-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("insert after the rejected attachment");
    assert_eq!(foreign_id, candidate, "the rejected controller keeps its own allocator");

    foreign.signal_shutdown().await;
    root.signal_shutdown().await;
}
