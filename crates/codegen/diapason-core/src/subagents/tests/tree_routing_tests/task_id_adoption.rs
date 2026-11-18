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

/// A subtree grafted through an intermediate scope still reaches the tree.
///
/// Three independently created controllers are joined in two steps: the leaf
/// is attached beneath the middle controller, so the leaf's slot now names the
/// middle controller's scope, and the middle controller is then attached
/// beneath the root. Redirecting only the slot the middle controller itself
/// holds would leave the leaf pointing at the middle controller's former
/// scope, free to repeat an id the root has issued since. The leaf must
/// therefore allocate from the root's scope through the middle scope's
/// forwarding link.
#[tokio::test]
async fn a_grafted_subtree_keeps_allocating_from_the_tree_it_joined() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("root controller");
    let spec = explorer_spec();

    let middle = nested_child_controller(workspace_root.clone(), "middle-session").await;
    let leaf = nested_child_controller(workspace_root, "leaf-session").await;

    // First graft: the leaf joins the middle controller while the pair is
    // still independent of the root.
    middle.state.write().await.children.insert(
        "leaf".to_string(),
        test_child_record("leaf", "leaf-session", "middle-session", &spec, SubagentStatus::Running, 2, None),
    );
    middle
        .attach_child_controller("leaf", Arc::clone(&leaf))
        .await
        .expect("the leaf joins the middle controller before the graft");

    // Second graft: the middle controller, and with it the leaf, joins the
    // root.
    root.state.write().await.children.insert(
        "middle".to_string(),
        test_child_record("middle", "middle-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    root.attach_child_controller("middle", Arc::clone(&middle))
        .await
        .expect("attach the middle controller");

    let shared = "agent-explorer-20261113T061900000Z";
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
    assert_eq!(root_id, shared, "the tree takes the free candidate");

    let leaf_id = leaf
        .insert_child_with_candidate_id(
            shared,
            test_child_record(shared, "leaf-session-candidate", "leaf-session", &spec, SubagentStatus::Queued, 3, None),
        )
        .await
        .expect("insert into the grafted leaf");
    assert_eq!(leaf_id, format!("{shared}-2"), "the grafted leaf must observe the id the root already issued");

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
        flat_len, 4,
        "the grafted tree holds the root's two tasks, the link into the middle controller and the leaf's \
         allocation; each must be listed once: {listed:?}"
    );
    assert_eq!(flat_len, listed.len(), "no task id may be listed twice");

    leaf.signal_shutdown().await;
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

/// A graft refuses a controller that is bound to another adoption lock.
///
/// The graft holds the tree's adoption lock so no id can be reserved while the
/// two allocators are reconciled. A controller bound to a different lock
/// cannot be made exclusive that way: an allocation in the tree could run
/// between the merge and the redirect. The attachment therefore fails closed,
/// and the rejection is atomic like the duplicate-id case.
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
    let error = root
        .attach_child_controller("foreign", Arc::clone(&foreign))
        .await
        .expect_err("a controller bound to another adoption lock cannot be grafted");
    assert!(format!("{error}").contains("lock scope"), "the rejection must name the scope mismatch: {error}");

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

/// A graft and an allocation that run at the same time never repeat a task id.
///
/// The graft copies the joining subtree's reserved ids into the tree's
/// allocator and then rewrites the subtree's slot to point at it. An allocation
/// that resolves the subtree's allocator before that rewrite but records its id
/// after the copy would leave the tree without that id, so the tree would issue
/// it again and the adapter would list one task twice. The shared adoption lock
/// must therefore cover the whole reconciliation on both sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_graft_and_allocation_never_repeat_a_task_id() {
    const ROUNDS: usize = 8;
    const PRE_GRAFT_IDS: usize = 400;
    const RACING_ALLOCATIONS: usize = 200;
    const POST_GRAFT_ALLOCATIONS: usize = 200;

    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let candidate = "agent-explorer-20261118T053000000Z";

    for round in 0..ROUNDS {
        // A real nested tree shares one lock scope; giving both controllers the
        // same scope is what lets the graft be accepted while still binding the
        // test's own allocators rather than the process-global ones.
        let scope = Arc::new(SubagentLockScope::new());
        let mut root_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
        root_config.lock_scope = Some(Arc::clone(&scope));
        let root = Arc::new(SubagentController::new(root_config).await.expect("root controller"));

        let joining_session = format!("joining-session-{round}");
        let mut joining_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
        joining_config.parent_session_id = joining_session.clone();
        joining_config.depth = 1;
        joining_config.managed_background_runtime = true;
        joining_config.lock_scope = Some(Arc::clone(&scope));
        let joining = Arc::new(SubagentController::new(joining_config).await.expect("joining controller"));
        let spec = Arc::new(explorer_spec());

        // Pre-fill the joining subtree so the graft's merge has real work to do
        // while the other thread reserves ids.
        for ordinal in 0..PRE_GRAFT_IDS {
            let id = format!("pre-graft-{round}-{ordinal}");
            let issued = joining
                .insert_child_with_candidate_id(
                    &id,
                    test_child_record(
                        &id,
                        &format!("{id}-session"),
                        &joining_session,
                        spec.as_ref(),
                        SubagentStatus::Queued,
                        2,
                        None,
                    ),
                )
                .await
                .expect("pre-fill the joining subtree");
            assert_eq!(issued, id, "a distinct pre-graft id must be issued verbatim");
        }

        root.state.write().await.children.insert(
            "joining".to_string(),
            test_child_record(
                "joining",
                "joining-session",
                "parent-session",
                spec.as_ref(),
                SubagentStatus::Running,
                1,
                None,
            ),
        );

        // A second worker thread reserves ids from the joining subtree while the
        // graft reconciles the two allocators.
        let racing = {
            let joining = Arc::clone(&joining);
            let spec = Arc::clone(&spec);
            let joining_session = joining_session.clone();
            tokio::spawn(async move {
                let mut issued = Vec::with_capacity(RACING_ALLOCATIONS);
                for ordinal in 0..RACING_ALLOCATIONS {
                    let session = format!("{joining_session}-racing-{ordinal}");
                    issued.push(
                        joining
                            .insert_child_with_candidate_id(
                                candidate,
                                test_child_record(
                                    candidate,
                                    &session,
                                    &joining_session,
                                    spec.as_ref(),
                                    SubagentStatus::Queued,
                                    2,
                                    None,
                                ),
                            )
                            .await
                            .expect("allocate while the graft runs"),
                    );
                }
                issued
            })
        };

        root.attach_child_controller("joining", Arc::clone(&joining))
            .await
            .expect("the joining subtree shares the tree's adoption lock");
        let mut issued = racing.await.expect("the racing allocator must finish");

        // Every id the subtree reserved during the graft is now known to the
        // tree, so proposing the same candidate again must not repeat any of
        // them. Sweeping the whole suffix range re-issues any id the tree
        // failed to learn.
        for ordinal in 0..POST_GRAFT_ALLOCATIONS {
            let session = format!("{joining_session}-after-{ordinal}");
            issued.push(
                joining
                    .insert_child_with_candidate_id(
                        candidate,
                        test_child_record(
                            candidate,
                            &session,
                            &joining_session,
                            spec.as_ref(),
                            SubagentStatus::Queued,
                            2,
                            None,
                        ),
                    )
                    .await
                    .expect("allocate after the graft"),
            );
        }

        let mut seen = std::collections::BTreeSet::new();
        let repeated = issued
            .iter()
            .filter(|id| !seen.insert((*id).clone()))
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            repeated.is_empty(),
            "round {round} issued these task ids more than once across the graft: {repeated:?}"
        );

        joining.signal_shutdown().await;
        root.signal_shutdown().await;
    }
}
