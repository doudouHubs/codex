use super::*;
use pretty_assertions::assert_eq;
use std::ffi::OsString;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn process_records_use_camel_case_wire_names() {
    let record = ProcessRecord {
        id: Uuid::nil(),
        pid: 42,
        parent_pid: Some(1),
        kind: ProcessKind::Tui,
        status: ProcessStatus::Running,
        activity: ActivityStatus::Thinking,
        mode: ProcessMode::Plan,
        summary: Some("planning".to_string()),
        error: None,
        executable: PathBuf::from("codex"),
        argv: vec!["--help".to_string()],
        cwd: PathBuf::from("."),
        thread_id: Some("thread-1".to_string()),
        created_at: 1,
        last_heartbeat_at: 2,
        last_state_update_at: 2,
        exit_code: None,
    };
    let json = serde_json::to_value(record).expect("record serializes");
    assert_eq!(json["parentPid"], 1);
    assert_eq!(json["lastHeartbeatAt"], 2);
    assert_eq!(json["threadId"], "thread-1");
}

#[test]
fn worker_arguments_are_filtered_from_clap_and_dashboard_argv() {
    let id = Uuid::new_v4();
    let lease_token = Uuid::new_v4();
    let (identity, args) = identity::parse_worker_args([
        OsString::from("codex"),
        OsString::from(SUPERVISOR_WORKER_ARG),
        OsString::from(id.to_string()),
        OsString::from(lease_token.to_string()),
        OsString::from("resume"),
        OsString::from("--last"),
    ])
    .expect("worker arguments parse");
    assert_eq!(identity, Some(WorkerIdentity { id, lease_token }));
    assert_eq!(
        args,
        [
            OsString::from("codex"),
            OsString::from("resume"),
            OsString::from("--last")
        ]
    );
}

#[test]
fn malformed_worker_arguments_fail_closed() {
    let result = identity::parse_worker_args([
        OsString::from("codex"),
        OsString::from(SUPERVISOR_WORKER_ARG),
        OsString::from("not-a-uuid"),
        OsString::from("also-not-a-uuid"),
    ]);
    assert!(result.is_err());
}

#[test]
fn frame_size_is_bounded() {
    assert_eq!(MAX_FRAME_SIZE, 1024 * 1024);
}

#[test]
fn work_pages_are_bounded_and_cursored() {
    let id = Uuid::new_v4();
    let details = WorkerDetails {
        prompt: Some("inspect the worker registry".to_string()),
        plan: vec![PlanStep {
            step: "read status".to_string(),
            status: "in_progress".to_string(),
        }],
        messages: vec![WorkMessage {
            role: "user".to_string(),
            content: "show all processes".to_string(),
            created_at: None,
        }],
        tool_calls: Vec::new(),
    };

    let page = details
        .page(id, WorkSection::Messages, None, 1)
        .expect("first page should be readable");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.items[0].content, "show all processes");
}

#[test]
fn work_pages_fit_the_transport_frame_limit() {
    let id = Uuid::new_v4();
    let details = WorkerDetails {
        tool_calls: (0..types::MAX_PAGE_LIMIT)
            .map(|index| ToolCallRecord {
                name: format!("tool-{index}"),
                input: "i".repeat(32 * 1024),
                output: Some("o".repeat(32 * 1024)),
                status: "completed".to_string(),
                created_at: None,
            })
            .collect(),
        ..WorkerDetails::default()
    };

    let page = details
        .page(id, WorkSection::ToolCalls, None, types::MAX_PAGE_LIMIT)
        .expect("large work pages should be split before exceeding the frame limit");
    let wire_size = serde_json::to_vec(&protocol::Response::WorkPage(page.clone()))
        .expect("work page should serialize")
        .len();

    assert!(wire_size <= MAX_FRAME_SIZE);
    assert!(page.items.len() < types::MAX_PAGE_LIMIT as usize);
    assert_eq!(page.next_cursor, Some(page.items.len().to_string()));
}
