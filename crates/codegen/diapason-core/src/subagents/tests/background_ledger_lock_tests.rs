//! Regressions for the workspace background ledger's inter-process lock.
//!
//! The lock is advisory and held per open file description, so a fresh handle
//! proves the operating-system lock is held rather than only an in-process
//! mutex. Acquisition is also bounded, so a stalled holder cannot park a save,
//! and the in-process mutex it holds, forever.
use super::*;
use crate::subagents::background::background_state_path_for_tests;

/// The ledger lock must exclude a second opener, not only a second task in
/// this process.
///
/// `flock` locks are held per open file description, so a fresh handle that
/// cannot take the lock proves the operating-system lock is held rather than
/// only the in-process mutex. Two Diapason processes sharing a workspace rely
/// on exactly that exclusion.
#[tokio::test]
async fn background_state_lock_excludes_a_second_opener() {
    let temp = TempDir::new().expect("tempdir");
    let guard = lock_background_state(temp.path())
        .await
        .expect("acquire the workspace ledger lock");
    // The lock is a sidecar next to the ledger, so derive it from the same
    // helper the tests use to locate the ledger rather than rebuilding the
    // workspace-relative path by hand.
    let path = background_state_path_for_tests(temp.path()).with_file_name("background_subagents.lock");
    let second_opener = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open the ledger lock file a second time");
    assert!(
        fs2::FileExt::try_lock_exclusive(&second_opener).is_err(),
        "a second opener must not acquire the ledger lock while the guard holds it"
    );
    drop(guard);
    fs2::FileExt::try_lock_exclusive(&second_opener).expect("the guard must release the ledger lock when it drops");
}

/// A contended ledger lock must fail with a contextual error at its deadline.
///
/// A blocking acquisition would park the save, and the in-process mutex it
/// holds, for as long as the holder lives; a stalled process would therefore
/// block every later save in this one. The deadline is shortened so the test
/// observes the expiry without waiting for the production timeout.
///
/// The holder is released by an independent thread so a regression that
/// removes the deadline cannot hang the test runner: without that release the
/// second acquisition would wait on a guard that only drops once the await has
/// returned.
#[tokio::test]
async fn background_state_lock_times_out_instead_of_blocking() {
    let temp = TempDir::new().expect("tempdir");
    let guard = lock_background_state(temp.path())
        .await
        .expect("acquire the workspace ledger lock");

    // The watchdog drops the guard as soon as the test asks, and also after a
    // bounded fallback, so a deadline regression fails the test rather than
    // hanging it while the guard stays held.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        let _ = release_rx.recv_timeout(Duration::from_secs(2));
        drop(guard);
    });

    let started = std::time::Instant::now();
    let error = match lock_background_state_within(temp.path(), Duration::from_millis(50)).await {
        Ok(_) => panic!("a lock held by another opener must not be acquired before its release"),
        Err(error) => error,
    };
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "the deadline must expire before the holder is released, took {elapsed:?}"
    );
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("Timed out") && rendered.contains("background_subagents.lock"),
        "the deadline error must name the wait and the lock file, got: {rendered}"
    );

    release_tx.send(()).expect("signal the holder to release the lock");
    watchdog.join().expect("watchdog thread");
    let _reacquired = lock_background_state(temp.path())
        .await
        .expect("the released lock must be acquirable");
}
