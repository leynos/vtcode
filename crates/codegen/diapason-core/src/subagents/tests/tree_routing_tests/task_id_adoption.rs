//! Task-ID adoption: attaching nested subtrees and rejecting irreconcilable ids.
use super::*;

/// Attaching a controller that already has descendants redirects all of them.
///
/// The attached controller and every controller it had already created share
/// one allocator slot. Adoption rewrites that slot, so the whole existing
/// subtree moves with the controller. Redirecting only the attached controller
/// would leave its deepest descendant allocating from the stale allocator,
/// free to propose an id the tree has since issued.
#[tokio::test]
async fn attaching_a_nested_controller_redirects_its_descendants() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("root controller");
    let spec = explorer_spec();

    // Build `middle` with a live `deepest` child the way the nested-delegation
    // loop does, then leave the pair independent of `root`.
    let middle = nested_child_controller(workspace_root.clone(), "middle-session").await;
    let deepest = nested_child_controller(workspace_root, "deepest-session").await;
    deepest.inherit_child_id_scope(&middle);
    middle.state.write().await.children.insert(
        "deepest".to_string(),
        test_child_record("deepest", "deepest-session", "middle-session", &spec, SubagentStatus::Running, 2, None),
    );
    middle
        .attach_child_controller("deepest", Arc::clone(&deepest))
        .await
        .expect("the nested controller joins its own parent before the graft");

    // The id the joining subtree already publishes is the one the tree must
    // treat as taken once the graft completes.
    let shared = "agent-explorer-20261113T061700000Z";
    let deepest_id = deepest
        .insert_child_with_candidate_id(
            shared,
            test_child_record(
                shared,
                "deepest-session-candidate",
                "deepest-session",
                &spec,
                SubagentStatus::Queued,
                3,
                None,
            ),
        )
        .await
        .expect("insert into the deepest descendant before the graft");
    assert_eq!(deepest_id, shared, "the independent subtree starts with a free candidate");

    root.state.write().await.children.insert(
        "middle".to_string(),
        test_child_record("middle", "middle-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    root.attach_child_controller("middle", Arc::clone(&middle))
        .await
        .expect("attach the populated subtree");

    let root_id = root
        .insert_child_with_candidate_id(
            shared,
            test_child_record(
                shared,
                "parent-session-candidate",
                "parent-session",
                &spec,
                SubagentStatus::Queued,
                1,
                None,
            ),
        )
        .await
        .expect("insert into the tree after the graft");
    assert_eq!(root_id, format!("{shared}-2"), "the tree must reserve the id the joining subtree already published");

    let descendant_id = deepest
        .insert_child_with_candidate_id(
            shared,
            test_child_record(
                shared,
                "deepest-session-candidate-2",
                "deepest-session",
                &spec,
                SubagentStatus::Queued,
                3,
                None,
            ),
        )
        .await
        .expect("the deepest descendant allocates after the graft");
    assert_eq!(
        descendant_id,
        format!("{shared}-3"),
        "the deepest descendant must allocate from the tree its ancestor joined"
    );

    // The flattened listing is what the adapter serves from the grafted tree.
    let mut listed = root
        .status_entries()
        .await
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    for controller in root.descendant_controllers().await {
        listed.extend(controller.status_entries().await.into_iter().map(|entry| entry.id));
    }
    listed.sort_unstable();
    let flat_len = listed.len();
    listed.dedup();
    assert_eq!(
        flat_len, 5,
        "the grafted tree holds the root's two tasks, the link into the subtree, the id the subtree \
         brought in and its post-graft allocation; each must be listed once: {listed:?}"
    );
    assert_eq!(flat_len, listed.len(), "no task id may be listed twice");

    deepest.signal_shutdown().await;
    middle.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// An id held by both the tree and a joining controller is rejected whole.
///
/// Suffixing cannot separate two ids that have already been published on both
/// sides, and merging the sets silently would leave the adapter listing one
/// task twice and routing a bare id to whichever controller it reached first.
/// Attachment therefore fails before any state changes.
#[tokio::test]
async fn attaching_controllers_that_share_an_id_is_rejected_without_mutation() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("root controller");
    let spec = explorer_spec();
    let grafted = nested_child_controller(workspace_root, "grafted-child-session").await;

    let shared = "agent-explorer-20261113T061800000Z";
    let root_id = root
        .insert_child_with_candidate_id(
            shared,
            test_child_record(
                shared,
                "parent-session-candidate",
                "parent-session",
                &spec,
                SubagentStatus::Queued,
                1,
                None,
            ),
        )
        .await
        .expect("insert into the tree");
    let grafted_id = grafted
        .insert_child_with_candidate_id(
            shared,
            test_child_record(
                shared,
                "grafted-session-candidate",
                "grafted-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("insert into the independent controller");
    assert_eq!(root_id, shared, "the tree holds the candidate");
    assert_eq!(grafted_id, shared, "the independent controller has its own allocator, so it holds it too");

    root.state.write().await.children.insert(
        "grafted".to_string(),
        test_child_record(
            "grafted",
            "grafted-child-session",
            "parent-session",
            &spec,
            SubagentStatus::Running,
            1,
            None,
        ),
    );
    let error = root
        .attach_child_controller("grafted", Arc::clone(&grafted))
        .await
        .expect_err("an id held on both sides cannot be reconciled by suffixing");
    assert!(format!("{error}").contains(shared), "the rejection must name the conflicting id: {error}");

    // Rejection is atomic: the record stays unattached, so the subtree is not
    // reachable from the tree, and the grafted controller keeps its own
    // allocator rather than being half-merged into the tree's.
    assert!(
        root.state
            .read()
            .await
            .children
            .get("grafted")
            .expect("the tracked record survives rejection")
            .child_controller
            .is_none(),
        "a rejected attachment must not publish the controller"
    );
    assert!(
        root.descendant_controllers().await.is_empty(),
        "a rejected attachment must not make the grafted subtree reachable"
    );
    let resolved = root
        .status_entries()
        .await
        .into_iter()
        .filter(|entry| entry.id == shared)
        .collect::<Vec<_>>();
    assert_eq!(resolved.len(), 1, "the shared id must resolve exactly once in the tree");
    assert_eq!(
        resolved[0].session_id, "parent-session-candidate",
        "the shared id must still resolve to the record that held it before the rejected attachment"
    );
    let grafted_second = grafted
        .insert_child_with_candidate_id(
            shared,
            test_child_record(
                shared,
                "grafted-session-candidate-2",
                "grafted-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("insert after the rejected attachment");
    assert_eq!(
        grafted_second,
        format!("{shared}-2"),
        "the rejected controller must still allocate from its own scope"
    );

    grafted.signal_shutdown().await;
    root.signal_shutdown().await;
}
