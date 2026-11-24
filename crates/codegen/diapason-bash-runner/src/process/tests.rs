use super::*;

use std::sync::atomic::AtomicUsize;

struct NoopTerminator;
impl ChildTerminator for NoopTerminator {
    fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Fails the first `failures_remaining` kill attempts.
struct FailingTerminator {
    failures_remaining: usize,
    attempts: Arc<AtomicUsize>,
}

impl ChildTerminator for FailingTerminator {
    fn kill(&mut self) -> io::Result<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.failures_remaining > 0 {
            self.failures_remaining -= 1;
            return Err(io::Error::other("simulated kill failure"));
        }
        Ok(())
    }
}

/// Reports the child as already gone, as `killpg` does for a reaped group.
struct GoneTerminator;

impl ChildTerminator for GoneTerminator {
    fn kill(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        }
        #[cfg(not(unix))]
        {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
}

fn test_handle(killer: Box<dyn ChildTerminator>) -> ProcessHandle {
    let (writer_tx, _writer_rx) = mpsc::channel(1);
    let (output_tx, initial_rx) = broadcast::channel(1);
    ProcessHandle::new(
        writer_tx,
        output_tx,
        initial_rx,
        killer,
        tokio::spawn(async {}),
        vec![tokio::spawn(async {}).abort_handle()],
        tokio::spawn(async {}),
        tokio::spawn(async {}),
        Arc::new(AtomicBool::new(false)),
        Arc::new(StdMutex::new(None)),
        None,
    )
    .0
}

fn assert_helper_tasks_aborted(handle: &ProcessHandle) {
    assert!(handle.reader_handle.lock().unwrap().is_none(), "reader handle must be aborted");
    assert!(handle.reader_abort_handles.lock().unwrap().is_empty(), "reader abort handles must be drained");
    assert!(handle.writer_handle.lock().unwrap().is_none(), "writer handle must be aborted");
    assert!(handle.wait_handle.lock().unwrap().is_none(), "wait handle must be aborted");
}

fn assert_helper_tasks_retained(handle: &ProcessHandle) {
    assert!(
        handle.reader_handle.lock().unwrap().is_some(),
        "a failed kill must leave the reader handle running for a later exit"
    );
    assert!(
        !handle.reader_abort_handles.lock().unwrap().is_empty(),
        "a failed kill must not drain the reader abort handles"
    );
    assert!(handle.writer_handle.lock().unwrap().is_some(), "a failed kill must leave the writer handle running");
    assert!(
        handle.wait_handle.lock().unwrap().is_some(),
        "a failed kill must leave the wait handle running so a later exit is observed"
    );
}

#[tokio::test]
async fn try_terminate_reports_a_kill_fault_and_keeps_the_killer_for_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let handle = test_handle(Box::new(FailingTerminator {
        failures_remaining: 1,
        attempts: Arc::clone(&attempts),
    }));

    let error = handle.try_terminate().expect_err("a kill fault must propagate");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "the first attempt must reach the killer");
    assert_helper_tasks_retained(&handle);

    // The failed kill restored the killer, so the retry reaches the child
    // instead of reporting a process it can no longer signal as gone.
    handle.try_terminate().expect("the retry after the fault must signal the child");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_helper_tasks_aborted(&handle);

    handle.try_terminate().expect("termination must stay idempotent");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn terminate_swallows_a_kill_fault_and_stays_infallible() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let handle = test_handle(Box::new(FailingTerminator {
        failures_remaining: 2,
        attempts: Arc::clone(&attempts),
    }));

    handle.terminate();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_helper_tasks_retained(&handle);

    // A retry remains available and stays infallible.
    handle.terminate();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_helper_tasks_retained(&handle);

    // Once a kill succeeds, the helpers are released.
    handle.terminate();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_helper_tasks_aborted(&handle);
}

/// A failed kill must not hide a child that exits afterwards.
///
/// The pipe backend's wait task is the only writer of `exit_status`, so
/// aborting it on a kill fault leaves a retained session reporting a live
/// process forever. The kill is made to fail against a real pipe-backed
/// child; the child then exits on its own and the still-running wait task
/// must record that exit.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_kill_keeps_the_wait_task_so_a_later_exit_is_observed() -> anyhow::Result<()> {
    let attempts = Arc::new(AtomicUsize::new(0));
    let options = crate::pipe::PipeSpawnOptions::new("/bin/sh", std::env::temp_dir()).args(["-c", "sleep 5"]);
    let spawned = crate::pipe::spawn_process_with_options(options).await?;
    let handle = spawned.session;
    {
        let mut killer = handle.killer.lock().unwrap();
        *killer = Some(Box::new(FailingTerminator {
            failures_remaining: 1,
            attempts: Arc::clone(&attempts),
        }));
    }

    let error = handle.try_terminate().expect_err("the injected kill fault must propagate");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "the injected killer must be reached");
    assert_helper_tasks_retained(&handle);
    assert!(!handle.has_exited(), "a failed kill must not report a live child as exited");

    tokio::time::timeout(tokio::time::Duration::from_secs(15), async {
        while !handle.has_exited() {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the retained wait task must observe the child's own exit");
    assert_eq!(handle.exit_code(), Some(0));
    Ok(())
}

#[tokio::test]
async fn try_terminate_treats_an_already_gone_child_as_success() {
    let handle = test_handle(Box::new(GoneTerminator));

    handle
        .try_terminate()
        .expect("a child that is already gone must count as terminated");
    assert_helper_tasks_aborted(&handle);
}

#[tokio::test]
async fn test_process_handle_debug() {
    // Just verify Debug impl doesn't panic
    let exit_status = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(StdMutex::new(None));

    let (writer_tx, _) = mpsc::channel(1);
    let (output_tx, initial_rx) = broadcast::channel(1);

    let (handle, _) = ProcessHandle::new(
        writer_tx,
        output_tx,
        initial_rx,
        Box::new(NoopTerminator),
        tokio::spawn(async {}),
        vec![],
        tokio::spawn(async {}),
        tokio::spawn(async {}),
        exit_status,
        exit_code,
        None,
    );

    let debug_str = format!("{handle:?}");
    assert!(debug_str.contains("ProcessHandle"));
}

#[tokio::test]
async fn test_has_exited() {
    let exit_status = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(StdMutex::new(None));

    let (writer_tx, _) = mpsc::channel(1);
    let (output_tx, initial_rx) = broadcast::channel(1);

    let (handle, _) = ProcessHandle::new(
        writer_tx,
        output_tx,
        initial_rx,
        Box::new(NoopTerminator),
        tokio::spawn(async {}),
        vec![],
        tokio::spawn(async {}),
        tokio::spawn(async {}),
        Arc::clone(&exit_status),
        exit_code,
        None,
    );

    assert!(!handle.has_exited());
    exit_status.store(true, Ordering::SeqCst);
    assert!(handle.has_exited());
}
