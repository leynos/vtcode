//! Task-ID allocation: tree-wide allocation across sibling and attached controllers.
use super::*;

/// Sibling controllers of one session must not allocate the same task id.
///
/// A task id is generated from the agent kind and a millisecond clock, so two
/// controllers that spawn the same kind in the same millisecond propose the
/// same id. The adapter lists the whole controller tree and routes a bare id
/// to the first controller that claims it, so a duplicate would list one task
/// twice and could resolve an output read or a cancel to the wrong controller.
/// Each controller's own `children` map cannot see the other's allocation, so
/// the check has to be tree-wide.
#[tokio::test]
async fn colliding_child_ids_stay_distinct_across_sibling_controllers() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("root controller");

    // Build the siblings the way `child_controller_for` does, including the
    // scope inheritance that gives them one shared allocator.
    let first = SubagentController::new({
        let mut config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
        config.parent_session_id = "first-child-session".to_string();
        config.depth = 1;
        config.managed_background_runtime = true;
        config
    })
    .await
    .expect("first sibling controller");
    first.inherit_child_id_scope(&root);
    let second = SubagentController::new({
        let mut config = test_controller_config(workspace_root, DiapasonConfig::default());
        config.parent_session_id = "second-child-session".to_string();
        config.depth = 1;
        config.managed_background_runtime = true;
        config
    })
    .await
    .expect("second sibling controller");
    second.inherit_child_id_scope(&root);

    let spec = explorer_spec();
    let candidate = "agent-explorer-20261113T060819731Z";
    let first_id = first
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "first-session-candidate",
                "first-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("first insertion");
    let second_id = second
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "second-session-candidate",
                "second-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("second insertion");

    assert_eq!(first_id, candidate, "the first allocation keeps the candidate id");
    assert_eq!(second_id, format!("{candidate}-2"), "the second takes the next free suffix");

    // The flattened listing is what the adapter serves: every task must appear
    // once, under the id its own controller stores.
    let mut entries = Vec::new();
    for controller in [&first, &second] {
        entries.extend(controller.status_entries().await);
    }
    let mut listed = entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>();
    let flat_len = listed.len();
    listed.sort_unstable();
    listed.dedup();
    assert_eq!(flat_len, 2, "both siblings must be listed");
    assert_eq!(flat_len, listed.len(), "no task id may be listed twice");

    // Lifecycle identity follows the id, so the two records stay separable in
    // the event stream rather than sharing one session key.
    let second_entry = second
        .status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == second_id)
        .expect("the suffixed id must be listed by its own controller");
    assert_eq!(second_entry.session_id, format!("second-child-session-{second_id}"));
    assert!(
        !second_entry.session_id.contains("second-session-candidate"),
        "the rewritten session id must not keep the pre-allocation name"
    );

    // Routing: a bare id now resolves in exactly one controller, which is what
    // the adapter's first-match lookup depends on for output and cancellation.
    for (id, owner, other) in [(&first_id, &first, &second), (&second_id, &second, &first)] {
        assert!(
            owner.status_entries().await.iter().any(|entry| entry.id == *id),
            "{id} must resolve in the controller that allocated it"
        );
        assert!(
            !other.status_entries().await.iter().any(|entry| entry.id == *id),
            "{id} must not also resolve in a sibling controller"
        );
    }

    first.signal_shutdown().await;
    second.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// Attaching a controller built elsewhere must put it on the tree's allocator.
///
/// `attach_child_controller` is the public seam that grafts a controller the
/// caller created itself into a session's tree. If that seam kept the
/// controller's private allocator, two independently created controllers
/// attached to the same tree could still propose the same task id, and the
/// adapter - which lists the whole tree and routes a bare id to the first
/// controller that claims it - would list one task twice and could send an
/// output read or a cancel to the wrong one.
#[tokio::test]
async fn attached_controllers_allocate_from_the_tree_scope() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("root controller");
    let spec = explorer_spec();

    // Neither controller has inherited anything: this is what a caller outside
    // the nested-delegation loop holds when it builds a controller itself.
    let first = nested_child_controller(workspace_root.clone(), "first-child-session").await;
    let second = nested_child_controller(workspace_root, "second-child-session").await;
    for (child_id, session_id, controller) in [
        ("first", "first-child-session", Arc::clone(&first)),
        ("second", "second-child-session", Arc::clone(&second)),
    ] {
        root.state.write().await.children.insert(
            child_id.to_string(),
            test_child_record(child_id, session_id, "parent-session", &spec, SubagentStatus::Running, 1, None),
        );
        root.attach_child_controller(child_id, controller)
            .await
            .expect("attach the independently created controller");
    }

    let candidate = "agent-explorer-20261113T061500000Z";
    let first_id = first
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "first-session-candidate",
                "first-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("first insertion");
    let second_id = second
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "second-session-candidate",
                "second-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("second insertion");

    assert_eq!(first_id, candidate, "the first allocation keeps the candidate id");
    assert_eq!(
        second_id,
        format!("{candidate}-2"),
        "attachment must share one allocator, so the second controller cannot repeat the candidate"
    );

    // The flattened listing is what the adapter serves from the attached tree.
    let mut listed = Vec::new();
    for controller in root.descendant_controllers().await {
        listed.extend(controller.status_entries().await.into_iter().map(|entry| entry.id));
    }
    listed.sort_unstable();
    let flat_len = listed.len();
    listed.dedup();
    assert_eq!(flat_len, 2, "both attached controllers must be reachable");
    assert_eq!(flat_len, listed.len(), "no attached task id may be listed twice");

    first.signal_shutdown().await;
    second.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// An attached controller keeps the ids its children already answer to.
///
/// A controller can spawn children before it is attached to a tree. Once it
/// joins, the new tree's allocator must still treat those ids as taken:
/// otherwise it would hand the same id to a different task, and the adapter's
/// first-match routing would resolve `output` and `cancel` for the newcomer to
/// the older, still-live task.
#[tokio::test]
async fn attaching_a_populated_controller_reserves_its_existing_ids() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("root controller");
    let spec = explorer_spec();
    let grafted = nested_child_controller(workspace_root, "grafted-child-session").await;

    let candidate = "agent-explorer-20261113T061600000Z";
    let pre_existing_id = grafted
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "grafted-session-candidate",
                "grafted-child-session",
                &spec,
                SubagentStatus::Queued,
                2,
                None,
            ),
        )
        .await
        .expect("insert before attachment");
    assert_eq!(pre_existing_id, candidate, "the unshared controller starts with a free candidate");

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
    root.attach_child_controller("grafted", Arc::clone(&grafted))
        .await
        .expect("attach the populated controller");

    let root_id = root
        .insert_child_with_candidate_id(
            candidate,
            test_child_record(
                candidate,
                "parent-session-candidate",
                "parent-session",
                &spec,
                SubagentStatus::Queued,
                1,
                None,
            ),
        )
        .await
        .expect("insert after attachment");

    assert_eq!(
        root_id,
        format!("{candidate}-2"),
        "the tree must not reissue an id a task brought in by attachment already holds"
    );
    assert!(
        grafted.status_entries().await.iter().any(|entry| entry.id == pre_existing_id),
        "the grafted subtree must keep its own child under its original id"
    );

    grafted.signal_shutdown().await;
    root.signal_shutdown().await;
}
