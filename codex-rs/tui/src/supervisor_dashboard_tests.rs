use super::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ListSelectionView;
use crate::keymap::RuntimeKeymap;
use crate::render::renderable::Renderable;
use codex_supervisor::ActivityStatus;
use codex_supervisor::ProcessKind;
use codex_supervisor::ProcessMode;
use codex_supervisor::ProcessRecord;
use codex_supervisor::ProcessStatus;
use codex_supervisor::SupervisorSnapshot;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::unbounded_channel;
use uuid::Uuid;

const SNAPSHOT_CURRENT_PID: u32 = 42_000;

fn render_params(params: SelectionViewParams, width: u16) -> String {
    let (raw_tx, _raw_rx) = unbounded_channel::<AppEvent>();
    let view = ListSelectionView::new(
        params,
        AppEventSender::new(raw_tx),
        RuntimeKeymap::defaults().list,
    );
    let area = Rect::new(0, 0, width, view.desired_height(width));
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    let rendered = (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    rendered
        .replace(&std::process::id().to_string(), "<current-pid>")
        .replace(&SNAPSHOT_CURRENT_PID.to_string(), "<current-pid>")
}

fn record(
    pid: u32,
    kind: ProcessKind,
    status: ProcessStatus,
    thread_id: Option<&str>,
) -> ProcessRecord {
    ProcessRecord {
        id: Uuid::from_u128(u128::from(pid)),
        pid,
        parent_pid: Some(1),
        kind,
        status,
        activity: ActivityStatus::Idle,
        mode: ProcessMode::Default,
        summary: None,
        error: None,
        executable: "codex".into(),
        argv: vec!["--model".to_string(), "gpt-5".to_string()],
        cwd: format!("/workspace/{pid}").into(),
        thread_id: thread_id.map(str::to_string),
        created_at: 100,
        last_observed_at: 200,
        last_state_update_at: 200,
        exit_code: None,
    }
}

#[test]
fn dashboard_snapshot_covers_current_and_managed_processes() {
    let snapshot = SupervisorSnapshot {
        protocol_version: 2,
        daemon_pid: 900,
        processes: vec![
            record(
                SNAPSHOT_CURRENT_PID,
                ProcessKind::Tui,
                ProcessStatus::Running,
                Some("thread-current"),
            ),
            record(
                42901,
                ProcessKind::Tui,
                ProcessStatus::Running,
                Some("thread-running"),
            ),
            record(42902, ProcessKind::Cli, ProcessStatus::Unresponsive, None),
            record(
                42903,
                ProcessKind::Tui,
                ProcessStatus::Stopping,
                Some("thread-stopping"),
            ),
        ],
    };

    insta::assert_snapshot!(
        "supervisor_dashboard_current_and_managed_processes",
        render_params(snapshot_params_for_pid(snapshot, SNAPSHOT_CURRENT_PID), 100)
    );
}

#[test]
fn dashboard_snapshot_handles_empty_registry() {
    let snapshot = SupervisorSnapshot {
        protocol_version: 2,
        daemon_pid: 901,
        processes: Vec::new(),
    };

    insta::assert_snapshot!(
        "supervisor_dashboard_empty_registry",
        render_params(snapshot_params(snapshot), 80)
    );
}

#[test]
fn dashboard_snapshot_handles_unavailable_supervisor() {
    insta::assert_snapshot!(
        "supervisor_dashboard_unavailable",
        render_params(
            error_params("failed to connect to supervisor socket".to_string()),
            80
        )
    );
}

#[test]
fn dashboard_snapshot_renders_process_details() {
    let mut process = record(
        42901,
        ProcessKind::Cli,
        ProcessStatus::Running,
        Some("thread-1"),
    );
    process.activity = ActivityStatus::ExecutingTool;
    process.mode = ProcessMode::Plan;
    process.summary = Some("cargo test -p codex-core".to_string());
    process.error = Some("previous retry".to_string());

    insta::assert_snapshot!(
        "supervisor_dashboard_process_details",
        render_params(process_details_params(process), 100)
    );
}

#[test]
fn dashboard_snapshot_renders_paginated_work_page() {
    let process_id = Uuid::from_u128(42901);
    let page = WorkPage {
        process_id,
        section: WorkSection::ToolCalls,
        items: vec![
            WorkItem {
                index: 0,
                title: "exec".to_string(),
                content: String::new(),
                id: Some("call-1".to_string()),
                status: Some("running".to_string()),
                input: Some("{\"command\":[\"cargo\",\"test\"]}".to_string()),
                output: None,
                created_at: Some(200),
            },
            WorkItem {
                index: 1,
                title: "apply_patch".to_string(),
                content: String::new(),
                id: Some("call-2".to_string()),
                status: Some("completed".to_string()),
                input: Some("{\"path\":\"src/lib.rs\"}".to_string()),
                output: Some("updated".to_string()),
                created_at: Some(201),
            },
        ],
        next_cursor: Some("2".to_string()),
        plan_text: None,
    };

    insta::assert_snapshot!(
        "supervisor_dashboard_paginated_work_page",
        render_params(work_page_params(page), 100)
    );
}

#[test]
fn dashboard_snapshot_renders_plan_text_and_checklist() {
    let process_id = Uuid::from_u128(42902);
    let page = WorkPage {
        process_id,
        section: WorkSection::Plan,
        items: vec![WorkItem {
            index: 0,
            title: "plan step".to_string(),
            content: "inspect the supervisor".to_string(),
            id: None,
            status: Some("inProgress".to_string()),
            input: None,
            output: None,
            created_at: None,
        }],
        next_cursor: None,
        plan_text: Some("1. inspect the supervisor\n2. report the result".to_string()),
    };

    insta::assert_snapshot!(
        "supervisor_dashboard_plan_text_and_checklist",
        render_params(work_page_params(page), 100)
    );
}
