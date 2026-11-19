//! The adoption and persistence locks are injectable per controller tree.
//!
//! A controller created without a scope must still share the process-global
//! default, preserving the cross-controller exclusion the former statics gave.
//! A controller given its own scope must not block a controller given a
//! different one, while two controllers sharing a scope must exclude each
//! other. The mutual exclusion is probed with `try_lock`, so the assertions are
//! deterministic rather than timing-dependent.
use super::*;

/// Builds a controller from the standard test config, optionally injecting the
/// tree's lock scope.
async fn controller_with_scope(
    workspace_root: PathBuf,
    lock_scope: Option<Arc<SubagentLockScope>>,
) -> SubagentController {
    let mut config = test_controller_config(workspace_root, DiapasonConfig::default());
    config.lock_scope = lock_scope;
    SubagentController::new(config).await.expect("controller")
}

/// Controllers that name no scope share the process-global default, so their
/// adoption and persistence locks still exclude one another.
#[tokio::test]
async fn controllers_without_an_injected_scope_share_the_process_default() {
    let first_temp = TempDir::new().expect("first tempdir");
    let second_temp = TempDir::new().expect("second tempdir");
    let first = controller_with_scope(first_temp.path().to_path_buf(), None).await;
    let second = controller_with_scope(second_temp.path().to_path_buf(), None).await;

    assert!(
        Arc::ptr_eq(first.lock_scope(), second.lock_scope()),
        "the default must be one process-global scope, not a fresh one per controller"
    );

    // The first guard must wait rather than probe: the default scope is
    // process-global, so another test that uses it can hold the lock for a
    // moment and a failed `try_lock` would not indicate a broken default.
    let guard = first
        .lock_scope()
        .adoption()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        second.lock_scope().adoption().try_lock().is_err(),
        "controllers without an injected scope must still exclude each other"
    );
    drop(guard);
}

/// Distinct injected scopes do not block each other; a shared scope does.
#[tokio::test]
async fn injected_lock_scopes_are_isolated_and_a_shared_scope_excludes() {
    let scope_a = Arc::new(SubagentLockScope::new());
    let scope_b = Arc::new(SubagentLockScope::new());
    let first_temp = TempDir::new().expect("first tempdir");
    let second_temp = TempDir::new().expect("second tempdir");
    let third_temp = TempDir::new().expect("third tempdir");
    let first = controller_with_scope(first_temp.path().to_path_buf(), Some(Arc::clone(&scope_a))).await;
    let second = controller_with_scope(second_temp.path().to_path_buf(), Some(Arc::clone(&scope_b))).await;
    let third = controller_with_scope(third_temp.path().to_path_buf(), Some(Arc::clone(&scope_a))).await;

    assert!(!Arc::ptr_eq(first.lock_scope(), second.lock_scope()), "distinct injected scopes must stay distinct");
    assert!(
        Arc::ptr_eq(first.lock_scope(), third.lock_scope()),
        "controllers given the same scope must share it"
    );

    let adoption = first.lock_scope().adoption().try_lock().expect("fresh adoption lock");
    assert!(second.lock_scope().adoption().try_lock().is_ok(), "a distinct scope must not block adoption");
    assert!(
        third.lock_scope().adoption().try_lock().is_err(),
        "one scope must exclude a second controller using it"
    );
    drop(adoption);

    let persistence = first.lock_scope().persistence().try_lock().expect("fresh persistence lock");
    assert!(second.lock_scope().persistence().try_lock().is_ok(), "a distinct scope must not block persistence");
    assert!(
        third.lock_scope().persistence().try_lock().is_err(),
        "one scope must exclude a second controller using it"
    );
    drop(persistence);
}

/// A child-scoped controller joins its parent's locks, exactly as it joins the
/// parent's child-id slot and progress observers.
#[tokio::test]
async fn child_controller_inherits_the_parent_lock_scope() {
    let parent_temp = TempDir::new().expect("parent tempdir");
    let child_temp = TempDir::new().expect("child tempdir");
    let parent =
        controller_with_scope(parent_temp.path().to_path_buf(), Some(Arc::new(SubagentLockScope::new()))).await;
    let mut child = controller_with_scope(child_temp.path().to_path_buf(), None).await;

    assert!(
        !Arc::ptr_eq(child.lock_scope(), parent.lock_scope()),
        "a fresh child controller starts on the process default"
    );

    child.inherit_lock_scope(&parent);

    assert!(
        Arc::ptr_eq(child.lock_scope(), parent.lock_scope()),
        "a child controller must join its parent tree's locks"
    );
}
