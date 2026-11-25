//! Which observers a grafted controller's snapshots reach.
//!
//! A controller can be given a route before it is attached to the tree it ends
//! up in, so these cases pin that a graft neither drops a registration an
//! adapter already holds nor routes a record to another session's observers.
//! Declared by `mod observer_ownership;` from `tests.rs`.

use super::*;

/// A controller keeps the observers registered before it joined a tree.
///
/// A child-scoped controller can be registered with before it is grafted: the
/// adapter that owns the session registers on the controller it was handed, and
/// the graft then replaces the route with the tree's. Replacing the registry
/// outright would drop those registrations, so the adapter would silently stop
/// receiving snapshots for a session it never detached from. The registration
/// travels into the adopted registry instead, and its handle still detaches it
/// from the registry that now holds it.
#[tokio::test]
async fn a_route_swap_keeps_the_observers_registered_before_it() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let nested = SubagentController::new(nested_controller_config(workspace_root, "child-session"))
        .await
        .expect("nested controller");

    // The adapter registers against the nested controller before the graft.
    let observer = Arc::new(RecordingObserver::default());
    let registration = nested.register_progress_observer("parent-session", observer.clone());

    nested.inherit_progress_observers(&root, "parent-session").await;

    let record = child_record("grandchild", "parent-session", &explorer_spec(), SubagentStatus::Running, 2);
    nested.state.write().await.children.insert("grandchild".to_string(), record);
    nested.publish_child_status("grandchild").await;

    {
        let updates = observer.children.lock().expect("children lock");
        assert_eq!(
            updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            vec!["grandchild"],
            "a registration made before the graft must keep receiving snapshots"
        );
    }

    // The handle detaches from the registry that now holds the registration,
    // so a later snapshot no longer reaches it.
    drop(registration);
    let later = child_record("later", "parent-session", &explorer_spec(), SubagentStatus::Running, 2);
    nested.state.write().await.children.insert("later".to_string(), later);
    nested.publish_child_status("later").await;
    {
        let updates = observer.children.lock().expect("children lock");
        assert_eq!(updates.len(), 1, "a detached registration must not receive later snapshots");
    }

    nested.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// A populated subtree publishing under another session is not grafted.
///
/// A controller that already owns records publishes them under the observer key
/// it adopted, and those records name the session that registered the adapter.
/// A graft rebinds the key to this tree's route, so a subtree owned by a
/// different session would deliver the older snapshots to this tree's observers
/// and to no one else. There is no way to re-key records after the fact, so the
/// attachment is refused before the route or the allocator is adopted, and the
/// existing link, route and records stay exactly as they were.
#[tokio::test]
async fn a_populated_subtree_owned_by_another_session_is_not_grafted() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let incoming = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "child-session"))
            .await
            .expect("incoming controller"),
    );
    let spec = explorer_spec();

    // The incoming controller already serves another session and holds a
    // record that publishes under it.
    incoming.inherit_progress_observers(&root, "other-session").await;
    let record = child_record("grandchild", "other-session", &spec, SubagentStatus::Running, 2);
    incoming.state.write().await.children.insert("grandchild".to_string(), record);
    let other_observer = Arc::new(RecordingObserver::default());
    let _other_registration = root.register_progress_observer("other-session", other_observer.clone());

    root.state
        .write()
        .await
        .children
        .insert("child".to_string(), child_record("child", "parent-session", &spec, SubagentStatus::Running, 1));

    let rejection = root
        .attach_child_controller("child", Arc::clone(&incoming))
        .await
        .expect_err("a subtree owned by another session must not be grafted into this one");
    assert!(
        format!("{rejection}").contains("publish under the other-session observer key"),
        "the rejection must name the owning session: {rejection}"
    );

    // The record and the incoming route are untouched: its snapshot still
    // reaches the observer registered for the session it already serves, and
    // nothing is published on the target tree's key.
    assert!(
        root.state
            .read()
            .await
            .children
            .get("child")
            .expect("the target record survives the rejection")
            .child_controller
            .is_none(),
        "a rejected graft must not publish the controller"
    );
    incoming.publish_child_status("grandchild").await;
    {
        let updates = other_observer.children.lock().expect("children lock");
        assert_eq!(
            updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            vec!["grandchild"],
            "a rejected graft must leave the incoming route on the session it already serves"
        );
    }

    incoming.signal_shutdown().await;
    root.signal_shutdown().await;
}
