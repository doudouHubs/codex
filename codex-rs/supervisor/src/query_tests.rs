use super::*;
use crate::ActivityStatus;
use crate::ProcessKind;
use crate::ProcessMode;
use crate::WorkerStatus;
use std::path::PathBuf;

fn record() -> ProcessRecord {
    ProcessRecord {
        id: Uuid::from_u128(7),
        pid: 70,
        parent_pid: Some(1),
        kind: ProcessKind::Cli,
        status: ProcessStatus::Starting,
        activity: ActivityStatus::Idle,
        mode: ProcessMode::Default,
        summary: None,
        error: None,
        executable: PathBuf::from("codex"),
        argv: vec!["exec".to_string()],
        cwd: PathBuf::from("."),
        thread_id: None,
        created_at: 1,
        last_observed_at: 2,
        last_state_update_at: 3,
        exit_code: None,
    }
}

#[test]
fn status_observation_merges_worker_state_into_the_record() {
    let mut actual = record();
    let mut expected = actual.clone();
    expected.status = ProcessStatus::Running;
    expected.activity = ActivityStatus::ExecutingTool;
    expected.mode = ProcessMode::Plan;
    expected.summary = Some("cargo test".to_string());
    expected.error = Some("previous attempt failed".to_string());
    expected.thread_id = Some("thread-7".to_string());
    expected.last_observed_at = 20;
    expected.last_state_update_at = 20;

    apply_observation(
        &mut actual,
        WorkerObservation::Status(WorkerStatus {
            thread_id: Some("thread-7".to_string()),
            activity: ActivityStatus::ExecutingTool,
            mode: ProcessMode::Plan,
            summary: Some("cargo test".to_string()),
            error: Some("previous attempt failed".to_string()),
            updated_at: 17,
        }),
        20,
    );

    assert_eq!(actual, expected);
}

#[test]
fn unresponsive_observation_preserves_last_worker_state() {
    let mut actual = record();
    actual.status = ProcessStatus::Running;
    actual.activity = ActivityStatus::Thinking;
    actual.mode = ProcessMode::Plan;
    actual.summary = Some("generating plan".to_string());
    actual.error = Some("last agent error".to_string());
    actual.thread_id = Some("thread-7".to_string());
    actual.last_state_update_at = 15;
    let mut expected = actual.clone();
    expected.status = ProcessStatus::Unresponsive;
    expected.last_observed_at = 30;

    apply_observation(
        &mut actual,
        WorkerObservation::Unresponsive("control channel closed".to_string()),
        30,
    );

    assert_eq!(actual, expected);
}

#[test]
fn exited_observation_wins_over_a_previous_lifecycle_state() {
    let mut actual = record();
    actual.status = ProcessStatus::Stopping;
    let mut expected = actual.clone();
    expected.status = ProcessStatus::Exited;
    expected.last_observed_at = 40;

    apply_observation(&mut actual, WorkerObservation::Exited, 40);

    assert_eq!(actual, expected);
}
