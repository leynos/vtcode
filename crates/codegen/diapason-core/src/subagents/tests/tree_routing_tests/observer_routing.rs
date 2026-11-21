//! Observer routing: descendant discovery and progress publication up the tree.
use super::*;

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

#[tokio::test]
async fn descendant_controllers_include_only_attached_child_scopes() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let spec = explorer_spec();
    let nested = nested_child_controller(temp.path().to_path_buf(), "child-session").await;

    {
        let mut state = parent.state.write().await;
        state.children.insert(
            "child".to_string(),
            test_child_record(
                "child",
                "child-session",
                "parent-session",
                &spec,
                SubagentStatus::Running,
                1,
                Some(Arc::clone(&nested)),
            ),
        );
        state.children.insert(
            "flat".to_string(),
            test_child_record("flat", "flat-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
        );
    }

    let descendants = parent.descendant_controllers().await;

    assert_eq!(descendants.len(), 1, "only attached child scopes are reachable");
    assert!(Arc::ptr_eq(&descendants[0], &nested));

    parent.signal_shutdown().await;
}

#[tokio::test]
async fn descendant_controllers_deduplicate_shared_child_scopes() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let spec = explorer_spec();
    let shared = nested_child_controller(temp.path().to_path_buf(), "first-child-session").await;

    {
        let mut state = parent.state.write().await;
        for id in ["first", "second"] {
            state.children.insert(
                id.to_string(),
                test_child_record(
                    id,
                    "first-child-session",
                    "parent-session",
                    &spec,
                    SubagentStatus::Running,
                    1,
                    Some(Arc::clone(&shared)),
                ),
            );
        }
    }

    assert_eq!(parent.descendant_controllers().await.len(), 1, "a shared child scope must be traversed once");

    parent.signal_shutdown().await;
}

#[tokio::test]
async fn attach_child_controller_rejects_unknown_tasks() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let nested = nested_child_controller(temp.path().to_path_buf(), "child-session").await;

    let err = parent
        .attach_child_controller("missing", nested)
        .await
        .expect_err("attaching to an unknown task must fail closed");

    assert!(err.to_string().contains("Unknown subagent id missing"), "{err}");

    parent.signal_shutdown().await;
}

#[tokio::test]
async fn nested_controller_publishes_under_the_root_observer_scope() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let nested = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
        config.parent_session_id = "child-session".to_string();
        config.depth = 1;
        config
    })
    .await
    .expect("nested controller");
    // The delegation was requested by the root session, so its events belong
    // to the observer registered for that session, not to the child's own.
    nested.inherit_progress_observers(&parent, "parent-session").await;

    let root_observer = Arc::new(RecordingObserver::default());
    let child_observer = Arc::new(RecordingObserver::default());
    let _root_registration = parent.register_progress_observer("parent-session", root_observer.clone());
    let _child_registration = parent.register_progress_observer("child-session", child_observer.clone());

    let spec = explorer_spec();
    {
        let mut state = nested.state.write().await;
        state.children.insert(
            "grandchild".to_string(),
            test_child_record(
                "grandchild",
                "grandchild-session",
                "child-session",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }

    nested.publish_child_status("grandchild").await;

    {
        let root_updates = root_observer.children.lock().expect("children lock");
        assert_eq!(root_updates.len(), 1, "root-scoped observers must see grandchild progress");
        assert_eq!(root_updates[0].id, "grandchild");
    }
    {
        let child_updates = child_observer.children.lock().expect("children lock");
        assert!(child_updates.is_empty(), "child-scoped keys must not receive aliased events");
    }

    // Shutdown publishes a terminal snapshot into the same observer, so the
    // assertions above must release the lock first.
    parent.signal_shutdown().await;
    nested.signal_shutdown().await;
}

#[tokio::test]
async fn nested_scope_is_inherited_through_more_than_one_level() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let first = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
        config.parent_session_id = "first-child-session".to_string();
        config.depth = 1;
        config
    })
    .await
    .expect("first nested controller");
    first.inherit_progress_observers(&parent, "parent-session").await;
    let second = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
        config.parent_session_id = "second-child-session".to_string();
        config.depth = 2;
        config
    })
    .await
    .expect("second nested controller");
    second.inherit_progress_observers(&first, "first-child-session").await;

    let root_observer = Arc::new(RecordingObserver::default());
    let _registration = parent.register_progress_observer("parent-session", root_observer.clone());

    let spec = explorer_spec();
    {
        let mut state = second.state.write().await;
        state.children.insert(
            "great-grandchild".to_string(),
            test_child_record(
                "great-grandchild",
                "great-grandchild-session",
                "second-child-session",
                &spec,
                SubagentStatus::Running,
                3,
                None,
            ),
        );
    }

    second.publish_child_status("great-grandchild").await;

    {
        let updates = root_observer.children.lock().expect("children lock");
        assert_eq!(updates.len(), 1, "every depth must publish under the root observer key");
        assert_eq!(updates[0].id, "great-grandchild");
    }

    // Shutdown publishes a terminal snapshot into the same observer, so the
    // assertion above must release the lock first.
    parent.signal_shutdown().await;
    first.signal_shutdown().await;
    second.signal_shutdown().await;
}

/// One controller can serve several host sessions at once.
///
/// The ACP adapter registers one observer per ACP session and builds child
/// controllers from a shared workspace controller. A child-scoped controller
/// must therefore key its events by the session that requested the delegation:
/// its own default parent session is only the controller's construction
/// context, and using it would deliver one session's grandchild snapshots to
/// another session's observer. Both nested controllers here carry the same
/// default parent session, so only the requesting session can separate them.
#[tokio::test]
async fn a_shared_controller_routes_each_sessions_delegations_to_its_own_observer() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let for_session_a = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
        config.parent_session_id = "controller-default-session".to_string();
        config.depth = 1;
        config
    })
    .await
    .expect("controller for session a");
    for_session_a.inherit_progress_observers(&parent, "session-a").await;
    let for_session_b = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
        config.parent_session_id = "controller-default-session".to_string();
        config.depth = 1;
        config
    })
    .await
    .expect("controller for session b");
    for_session_b.inherit_progress_observers(&parent, "session-b").await;

    let observer_a = Arc::new(RecordingObserver::default());
    let observer_b = Arc::new(RecordingObserver::default());
    let _registration_a = parent.register_progress_observer("session-a", observer_a.clone());
    let _registration_b = parent.register_progress_observer("session-b", observer_b.clone());

    let spec = explorer_spec();
    {
        let mut state = for_session_a.state.write().await;
        state.children.insert(
            "grandchild-a".to_string(),
            // The immediate parent is session a's own child session, not the
            // observer key. Routing by `parent_thread_id` would find no
            // observer here, so only the inherited observer scope reaches
            // session-a.
            test_child_record(
                "grandchild-a",
                "grandchild-a-session",
                "child-a-session",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }
    {
        let mut state = for_session_b.state.write().await;
        state.children.insert(
            "grandchild-b".to_string(),
            test_child_record(
                "grandchild-b",
                "grandchild-b-session",
                "child-b-session",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }

    for_session_a.publish_child_status("grandchild-a").await;
    for_session_b.publish_child_status("grandchild-b").await;

    {
        let updates = observer_a.children.lock().expect("children lock");
        assert_eq!(updates.len(), 1, "session-a must receive its own grandchild only");
        assert_eq!(updates[0].id, "grandchild-a");
    }
    {
        let updates = observer_b.children.lock().expect("children lock");
        assert_eq!(updates.len(), 1, "session-b must receive its own grandchild only");
        assert_eq!(updates[0].id, "grandchild-b");
    }

    // Shutdown publishes a terminal snapshot into the same observers, so the
    // assertions above must release the locks first.
    parent.signal_shutdown().await;
    for_session_a.signal_shutdown().await;
    for_session_b.signal_shutdown().await;
}

/// An independently constructed controller must publish through the registry
/// of the tree it joins, not through the one it was created with.
///
/// `inherit_progress_observers` is only called by native child construction, so
/// this exercises the attachment seam on its own: the incoming controller has
/// never inherited anything, and its grandchild's immediate parent session is
/// its own. Only the route adopted at attachment can reach the observer
/// registered on the root.
#[tokio::test]
async fn an_attached_controller_publishes_through_the_joining_trees_observers() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");
    let incoming = Arc::new(
        SubagentController::new({
            let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
            config.parent_session_id = "child-session".to_string();
            config.depth = 1;
            config
        })
        .await
        .expect("incoming controller"),
    );
    let spec = explorer_spec();
    {
        let mut state = parent.state.write().await;
        state.children.insert(
            "child".to_string(),
            test_child_record("child", "child-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
        );
    }

    let root_observer = Arc::new(RecordingObserver::default());
    let _registration = parent.register_progress_observer("parent-session", root_observer.clone());

    parent
        .attach_child_controller("child", Arc::clone(&incoming))
        .await
        .expect("attach the independently constructed controller");

    {
        let mut state = incoming.state.write().await;
        state.children.insert(
            "grandchild".to_string(),
            test_child_record(
                "grandchild",
                "grandchild-session",
                "child-session",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }
    incoming.publish_child_status("grandchild").await;

    {
        let updates = root_observer.children.lock().expect("children lock");
        assert_eq!(
            updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            vec!["grandchild"],
            "the attached subtree must publish through the root's observers"
        );
    }

    parent.signal_shutdown().await;
}
