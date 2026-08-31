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
        last_observed_at: 2,
        last_state_update_at: 2,
        exit_code: None,
    };
    let json = serde_json::to_value(record).expect("record serializes");
    assert_eq!(json["parentPid"], 1);
    assert_eq!(json["lastObservedAt"], 2);
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
        plan_text: Some("1. inspect\n2. report".to_string()),
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

    let plan_page = details
        .page(id, WorkSection::Plan, None, 1)
        .expect("plan page should be readable");
    assert_eq!(
        plan_page.plan_text,
        Some("1. inspect\n2. report".to_string())
    );
    assert_eq!(plan_page.items[0].content, "read status");
}

#[test]
fn reporter_merges_tool_lifecycle_and_exposes_plan_text() {
    let reporter = reporter::SupervisorReporter::new();
    let process_id = Uuid::new_v4();
    reporter.set_prompt("inspect the supervisor".to_string());
    reporter.set_plan_text("first".to_string());
    reporter.append_plan_delta("\nsecond".to_string());
    reporter.upsert_tool_call(ToolCallRecord {
        id: Some("call-1".to_string()),
        name: "exec".to_string(),
        input: "{\"command\":[\"pwd\"]}".to_string(),
        output: None,
        status: "running".to_string(),
        created_at: Some(1),
    });
    reporter.upsert_tool_call(ToolCallRecord {
        id: Some("call-1".to_string()),
        name: "exec".to_string(),
        input: String::new(),
        output: Some("workspace".to_string()),
        status: "completed".to_string(),
        created_at: Some(2),
    });

    let plan_page = reporter
        .page(process_id, WorkSection::Plan, None, 20)
        .expect("plan page should be readable");
    assert_eq!(plan_page.plan_text, Some("first\nsecond".to_string()));

    let tool_page = reporter
        .page(process_id, WorkSection::ToolCalls, None, 20)
        .expect("tool page should be readable");
    assert_eq!(tool_page.items.len(), 1);
    assert_eq!(tool_page.items[0].id, Some("call-1".to_string()));
    assert_eq!(tool_page.items[0].status, Some("completed".to_string()));
    assert_eq!(tool_page.items[0].output, Some("workspace".to_string()));
}

#[test]
fn newly_added_work_fields_are_optional_on_the_wire() {
    let process_id = Uuid::new_v4();
    let tool_call: ToolCallRecord = serde_json::from_value(serde_json::json!({
        "name": "exec",
        "input": "{}",
        "output": null,
        "status": "completed",
        "createdAt": 1,
    }))
    .expect("legacy tool call should remain readable");
    assert_eq!(tool_call.id, None);

    let page: WorkPage = serde_json::from_value(serde_json::json!({
        "processId": process_id,
        "section": "messages",
        "items": [{
            "index": 0,
            "title": "user",
            "content": "inspect the supervisor",
            "status": null,
            "input": null,
            "output": null,
            "createdAt": null,
        }],
        "nextCursor": null,
    }))
    .expect("legacy work page should remain readable");
    assert_eq!(page.plan_text, None);
    assert_eq!(page.items[0].id, None);
}

#[test]
fn work_pages_fit_the_transport_frame_limit() {
    let id = Uuid::new_v4();
    let details = WorkerDetails {
        tool_calls: (0..types::MAX_PAGE_LIMIT)
            .map(|index| ToolCallRecord {
                id: Some(format!("call-{index}")),
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
