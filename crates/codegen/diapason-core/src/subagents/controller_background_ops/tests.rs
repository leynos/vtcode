//! Exercises the controller side of the background output boundary.
//!
//! Each case pins one contract the controller relies on when it reads
//! through [`BackgroundOutputReader`]: a live tail keeps the requested
//! bound, a not-ready or archived task serves the recorded output, a
//! live-read fault is reported against its target instead of being masked
//! by an empty result, the preview applies its fixed bound, an unreadable
//! archive reaches the caller, and the reader resolves each task by task id.
//! A liveness decision is pinned separately: an observation only applies to
//! the launch it was taken from.
use super::controller_background_query::liveness_observation_is_current;
use super::{
    BackgroundOutput, BackgroundOutputError, BackgroundOutputReader, BackgroundSubprocessEntry,
    BackgroundSubprocessStatus, BackgroundTaskRef, exec_session_is_missing, read_output_tail, read_preview,
};
use futures::future::BoxFuture;
use rstest::rstest;

use crate::subagents::constants::SUBAGENT_PREVIEW_LINES;

mod abandoned_start_liveness_tests;
mod archive_fault_tests;
mod cancel_tests;
mod claim_conflict_tests;
mod close_fault_state_tests;
mod lifecycle_tests;
mod pending_write_tests;
mod query_tests;
mod replacement_tests;
mod reserve_support;
mod reserve_tests;
mod resolver_tests;
mod run_tests;
mod settlement_tests;
mod start_lock_fault_tests;
mod stop_close_outcome_tests;
mod stop_close_tests;
mod stop_intent_durability_tests;
pub(super) mod support;

const TARGET: &str = "background-worker";

#[derive(Clone, Copy)]
enum FakeOutput {
    Live(&'static str),
    Archived(&'static str),
    NotReady,
    LiveFailure(&'static str),
    ArchiveFailure(&'static str),
}

/// Scripts one outcome per task id, so the fake is keyed like the real
/// reader rather than by any storage handle.
struct FakeOutputReader {
    outputs: Vec<(&'static str, FakeOutput)>,
}

impl FakeOutputReader {
    /// Scripts one outcome for a single task id.
    fn for_task(task_id: &'static str, output: FakeOutput) -> Self {
        Self { outputs: vec![(task_id, output)] }
    }

    /// Scripts one outcome for each task id.
    fn with_outputs(outputs: Vec<(&'static str, FakeOutput)>) -> Self {
        Self { outputs }
    }
}

impl BackgroundOutputReader for FakeOutputReader {
    fn read_output<'a>(
        &'a self,
        task: &'a BackgroundTaskRef<'a>,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>> {
        let output = self
            .outputs
            .iter()
            .find_map(|(task_id, output)| (task.id == *task_id).then_some(*output));
        Box::pin(async move {
            match output {
                Some(FakeOutput::Live(output)) => Ok(BackgroundOutput::Live(output.to_string())),
                Some(FakeOutput::Archived(output)) => Ok(BackgroundOutput::Archived(output.to_string())),
                Some(FakeOutput::NotReady) | None => Ok(BackgroundOutput::NotReady),
                Some(FakeOutput::LiveFailure(message)) => {
                    Err(BackgroundOutputError::Live(anyhow::anyhow!("{message}")))
                }
                Some(FakeOutput::ArchiveFailure(message)) => {
                    Err(BackgroundOutputError::Archived(anyhow::anyhow!("{message}")))
                }
            }
        })
    }
}

fn entry_with_id(id: &str, transcript_path: Option<&str>) -> BackgroundSubprocessEntry {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "session_id": "session-background-worker",
        "exec_session_id": "exec-session-background-worker-1-1",
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "status": "running",
        "desired_enabled": false,
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "transcript_path": transcript_path
    }))
    .expect("background subprocess entry")
}

/// A liveness observation only describes the launch it was taken from.
///
/// The projection consults the launch id, the status, the desired flag and the
/// restart budget after reading the exec session outside the state lock. A
/// change to any of them means a cancellation, a restart, a commit or a
/// replacement landed in that window, so the captured outcome must not be
/// applied.
#[rstest]
#[case::unchanged("exec-launch", BackgroundSubprocessStatus::Running, true, 0, true)]
#[case::replaced_launch("exec-replacement", BackgroundSubprocessStatus::Running, true, 0, false)]
#[case::committed_status("exec-launch", BackgroundSubprocessStatus::Starting, true, 0, false)]
#[case::cancelled_launch("exec-launch", BackgroundSubprocessStatus::Running, false, 0, false)]
#[case::spent_restart("exec-launch", BackgroundSubprocessStatus::Running, true, 1, false)]
fn a_liveness_observation_is_stale_after_any_relevant_change(
    #[case] exec_session_id: &str,
    #[case] status: BackgroundSubprocessStatus,
    #[case] desired_enabled: bool,
    #[case] restart_attempts: u8,
    #[case] expected: bool,
) {
    let record = support::background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );

    assert_eq!(
        liveness_observation_is_current(&record, exec_session_id, status, desired_enabled, restart_attempts),
        expected,
        "the observation's relevance must follow the record it was taken from"
    );
}

/// Both output sources are bounded by the caller's line count.
///
/// The live tail and the archived transcript are read by different paths, so
/// each is pinned here: a bound applied in only one of them would still let a
/// caller receive more lines than it asked for from the other.
#[rstest]
#[case::live(FakeOutput::Live("one\ntwo\nthree"), 2, "two\nthree")]
#[case::archived(FakeOutput::Archived("old\narchived"), 1, "archived")]
#[tokio::test]
async fn output_tail_keeps_the_requested_bound(
    #[case] source: FakeOutput,
    #[case] lines: usize,
    #[case] expected: &str,
) {
    let reader = FakeOutputReader::for_task(TARGET, source);
    let entry = entry_with_id(TARGET, Some("/tmp/session.json"));

    let output = read_output_tail(&reader, &entry, TARGET, lines)
        .await
        .expect("output tail keeps the requested bound");

    assert_eq!(output, expected);
}

/// Two tasks with different outcomes resolve independently by task id.
#[tokio::test]
async fn each_task_resolves_independently_by_task_id() {
    let reader = FakeOutputReader::with_outputs(vec![
        ("first-task", FakeOutput::Live("first live")),
        ("second-task", FakeOutput::Archived("second archived")),
    ]);
    let first = entry_with_id("first-task", Some("/tmp/first.json"));
    let second = entry_with_id("second-task", Some("/tmp/second.json"));

    let first_tail = read_output_tail(&reader, &first, "first-task", 10)
        .await
        .expect("first task tail");
    let second_tail = read_output_tail(&reader, &second, "second-task", 10)
        .await
        .expect("second task tail");
    let second_preview = read_preview(&reader, &second, "second-task")
        .await
        .expect("second task preview");

    assert_eq!(first_tail, "first live");
    assert_eq!(second_tail, "second archived");
    assert_eq!(second_preview, "second archived");
}

/// A task with no output yet reads as an empty tail.
#[tokio::test]
async fn not_ready_output_is_empty() {
    let reader = FakeOutputReader::for_task(TARGET, FakeOutput::NotReady);
    let entry = entry_with_id(TARGET, None);

    let output = read_output_tail(&reader, &entry, TARGET, 10).await.expect("empty tail");

    assert!(output.is_empty());
}

/// A read fault names the task it was reading.
#[tokio::test]
async fn read_errors_are_reported_with_the_target() {
    let reader = FakeOutputReader::for_task(TARGET, FakeOutput::LiveFailure("permission denied"));
    let entry = entry_with_id(TARGET, Some("/tmp/session.json"));

    let error = read_output_tail(&reader, &entry, TARGET, 10)
        .await
        .expect_err("a read fault must reach the caller");

    let rendered = format!("{error:#}");
    assert!(rendered.contains("read background output for background-worker"), "{rendered}");
    assert!(rendered.contains("permission denied"), "{rendered}");
}

/// A preview applies the fixed preview bound.
#[tokio::test]
async fn preview_keeps_the_preview_bound() {
    let archived = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n25\n26";
    let reader = FakeOutputReader::for_task(TARGET, FakeOutput::Archived(archived));
    let entry = entry_with_id(TARGET, Some("/tmp/session.json"));

    let preview = read_preview(&reader, &entry, TARGET).await.expect("archived preview");

    let lines = preview.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), SUBAGENT_PREVIEW_LINES);
    assert_eq!(lines.first().copied(), Some("3"));
    assert_eq!(lines.last().copied(), Some("26"));
}

/// A preview reports the storage fault it met, whatever layer raised it.
///
/// The live session and the archive are different storage layers behind the
/// same reader, so both are exercised by one case table: a fault from either is
/// the same fact for a caller — the preview could not be read — and neither may
/// be masked as "no output yet". Each case supplies its own cause text, so a
/// case that silently stopped faulting would fail rather than pass on an
/// unrelated error.
#[rstest]
#[case::archive(FakeOutput::ArchiveFailure("archive is corrupt"), "archive is corrupt")]
#[case::live(FakeOutput::LiveFailure("permission denied"), "permission denied")]
#[tokio::test]
async fn preview_propagates_a_storage_read_fault(#[case] source: FakeOutput, #[case] cause: &str) {
    let reader = FakeOutputReader::for_task(TARGET, source);
    let entry = entry_with_id(TARGET, Some("/tmp/session.json"));

    let error = read_preview(&reader, &entry, TARGET)
        .await
        .expect_err("a storage fault must reach the caller instead of an empty preview");

    let rendered = format!("{error:#}");
    assert!(rendered.contains("read background output for background-worker"), "{rendered}");
    assert!(rendered.contains(cause), "the {cause} cause must survive the mapping: {rendered}");
}

/// Both storage layers report a vanished session as the expected exit.
///
/// The record is removed before the PTY handle is released, so a read that
/// races the close sees either cause depending on where it lands. Both are
/// the same fact — the session is gone — and neither is an observation
/// failure. A plain fault stays a fault, including when a caller wraps it
/// in context.
#[test]
fn liveness_classifier_treats_a_missing_backend_as_an_expected_exit() {
    let handle_gone =
        anyhow::Error::new(crate::tools::pty::PtySessionNotFound { session_id: "exec-background-worker".to_owned() });
    assert!(exec_session_is_missing(&handle_gone), "a backend that dropped the handle is the expected exit");

    let record_gone = anyhow::Error::new(crate::tools::exec_session::ExecSessionNotFound {
        session_id: "exec-background-worker".to_owned(),
    })
    .context("read background liveness");
    assert!(exec_session_is_missing(&record_gone), "the typed cause must be found through a context wrapper");

    let fault = anyhow::anyhow!("exec session store is unavailable");
    assert!(!exec_session_is_missing(&fault), "an untyped fault told the caller nothing about the process");
}
