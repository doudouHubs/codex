use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use codex_supervisor::ProcessKind;
use codex_supervisor::ProcessMode;
use codex_supervisor::ProcessRecord;
use codex_supervisor::ProcessStatus;
use codex_supervisor::SupervisorClient;
use codex_supervisor::SupervisorSnapshot;
use codex_supervisor::WorkItem;
use codex_supervisor::WorkPage;
use codex_supervisor::WorkSection;
use tokio::spawn;
use uuid::Uuid;

pub(crate) const VIEW_ID: &str = "supervisor-dashboard";

/// Dashboard 的查询只连接现有 supervisor；连接失败必须显式显示错误，不能在查询路径上拉起新 daemon。
pub(crate) fn request_snapshot(tx: AppEventSender) {
    spawn(async move {
        let result = match SupervisorClient::connect().await {
            Ok(client) => client
                .snapshot()
                .await
                .map_err(|error| format!("{error:#}")),
            Err(error) => Err(format!("{error:#}")),
        };
        tx.send(AppEvent::SupervisorSnapshotLoaded { result });
    });
}

pub(crate) fn request_process_details(tx: AppEventSender, id: Uuid) {
    spawn(async move {
        let result = match SupervisorClient::connect().await {
            Ok(client) => match client.snapshot().await {
                Ok(snapshot) => snapshot
                    .processes
                    .into_iter()
                    .find(|record| record.id == id)
                    .ok_or_else(|| format!("supervisor process {id} was not found")),
                Err(error) => Err(format!("{error:#}")),
            },
            Err(error) => Err(format!("{error:#}")),
        };
        tx.send(AppEvent::SupervisorProcessDetailsLoaded { result });
    });
}

pub(crate) fn request_work_page(
    tx: AppEventSender,
    id: Uuid,
    section: WorkSection,
    cursor: Option<String>,
) {
    spawn(async move {
        let result = match SupervisorClient::connect().await {
            Ok(client) => client
                .work_page(id, section, cursor, 20)
                .await
                .map_err(|error| format!("{error:#}")),
            Err(error) => Err(format!("{error:#}")),
        };
        tx.send(AppEvent::SupervisorWorkLoaded { result });
    });
}

pub(crate) fn request_termination(tx: AppEventSender, id: Uuid) {
    spawn(async move {
        let result = match SupervisorClient::connect().await {
            Ok(client) => client
                .terminate(id)
                .await
                .map_err(|error| format!("{error:#}")),
            Err(error) => Err(format!("{error:#}")),
        };
        tx.send(AppEvent::SupervisorProcessTerminated { id, result });
    });
}

pub(crate) fn loading_params() -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some("Codex process dashboard".to_string()),
        subtitle: Some("Loading process state...".to_string()),
        items: vec![SelectionItem {
            name: "Loading...".to_string(),
            is_disabled: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub(crate) fn error_params(error: String) -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some("Codex process dashboard".to_string()),
        subtitle: Some("Supervisor is unavailable".to_string()),
        items: vec![
            SelectionItem {
                name: "Retry connection".to_string(),
                description: Some(error),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::RefreshSupervisorDashboard);
                })],
                dismiss_on_select: false,
                ..Default::default()
            },
            back_to_dashboard_item(),
        ],
        ..Default::default()
    }
}

pub(crate) fn snapshot_params(snapshot: SupervisorSnapshot) -> SelectionViewParams {
    snapshot_params_for_pid(snapshot, std::process::id())
}

fn snapshot_params_for_pid(snapshot: SupervisorSnapshot, current_pid: u32) -> SelectionViewParams {
    let process_count = snapshot.processes.len();
    let mut items = vec![SelectionItem {
        name: "Refresh process list".to_string(),
        description: Some(format!(
            "Supervisor PID {} | {process_count} registered process(es)",
            snapshot.daemon_pid
        )),
        actions: vec![Box::new(|tx| {
            tx.send(AppEvent::RefreshSupervisorDashboard);
        })],
        dismiss_on_select: false,
        ..Default::default()
    }];

    if snapshot.processes.is_empty() {
        items.push(SelectionItem {
            name: "No Codex processes registered".to_string(),
            description: Some("The supervisor is running, but its registry is empty.".to_string()),
            is_disabled: true,
            ..Default::default()
        });
    } else {
        items.extend(
            snapshot
                .processes
                .into_iter()
                .map(|record| process_item(record, current_pid)),
        );
    }

    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some("Codex process dashboard".to_string()),
        subtitle: Some(format!(
            "Supervisor PID {} | {process_count} registered process(es)",
            snapshot.daemon_pid
        )),
        items,
        ..Default::default()
    }
}

pub(crate) fn process_loading_params(id: Uuid) -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some("Codex process details".to_string()),
        subtitle: Some(format!("Loading process {id}...")),
        items: vec![SelectionItem {
            name: "Loading process state...".to_string(),
            is_disabled: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub(crate) fn process_error_params(error: String) -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some("Codex process details".to_string()),
        subtitle: Some("Unable to read process state".to_string()),
        items: vec![
            SelectionItem {
                name: "Process query failed".to_string(),
                description: Some(error),
                is_disabled: true,
                ..Default::default()
            },
            back_to_dashboard_item(),
        ],
        ..Default::default()
    }
}

pub(crate) fn process_details_params(record: ProcessRecord) -> SelectionViewParams {
    let id = record.id;
    let current_pid = std::process::id();
    let is_current = record.pid == current_pid;
    let can_terminate = !is_current
        && !matches!(
            record.status,
            ProcessStatus::Stopping | ProcessStatus::Exited
        );
    let mut items = vec![
        SelectionItem {
            name: "Refresh process state".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenSupervisorProcessDetails { id });
            })],
            dismiss_on_select: false,
            ..Default::default()
        },
        back_to_dashboard_item(),
    ];

    let termination_description = if is_current {
        "The current Codex process cannot terminate itself.".to_string()
    } else if record.status == ProcessStatus::Stopping {
        "Termination is already in progress.".to_string()
    } else if record.status == ProcessStatus::Exited {
        "This process has already exited.".to_string()
    } else {
        "Supervisor will terminate this process and its child process group.".to_string()
    };
    let terminate_action: Vec<SelectionAction> = can_terminate
        .then(|| {
            Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::TerminateSupervisorProcess { id });
            }) as SelectionAction
        })
        .into_iter()
        .collect();
    items.push(SelectionItem {
        name: if can_terminate {
            "Terminate process".to_string()
        } else {
            "Process termination unavailable".to_string()
        },
        description: Some(termination_description),
        is_disabled: !can_terminate,
        actions: terminate_action,
        dismiss_on_select: can_terminate,
        ..Default::default()
    });

    for section in [
        WorkSection::Prompt,
        WorkSection::Plan,
        WorkSection::Messages,
        WorkSection::ToolCalls,
    ] {
        let section_id = section;
        items.push(SelectionItem {
            name: format!("Read {}", section_label(section)),
            description: Some(format!(
                "Open the paginated {} section for this process.",
                section_label(section)
            )),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::ReadSupervisorWork {
                    id,
                    section: section_id,
                    cursor: None,
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
    }

    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some(format!("{} process details", kind_label(record.kind))),
        subtitle: Some(process_details_description(&record)),
        items,
        ..Default::default()
    }
}

pub(crate) fn work_loading_params(id: Uuid, section: WorkSection) -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some(format!("Process {}: {}", id, section_label(section))),
        subtitle: Some("Loading work content...".to_string()),
        items: vec![SelectionItem {
            name: "Loading...".to_string(),
            is_disabled: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub(crate) fn work_error_params(error: String) -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some("Codex process work".to_string()),
        subtitle: Some("Unable to read work content".to_string()),
        items: vec![
            SelectionItem {
                name: "Work query failed".to_string(),
                description: Some(error),
                is_disabled: true,
                ..Default::default()
            },
            back_to_dashboard_item(),
        ],
        ..Default::default()
    }
}

pub(crate) fn work_page_params(page: WorkPage) -> SelectionViewParams {
    let id = page.process_id;
    let section = page.section;
    let next_cursor = page.next_cursor.clone();
    let mut items = vec![back_to_process_item(id)];
    if let Some(plan_text) = page.plan_text
        && !plan_text.is_empty()
    {
        items.push(SelectionItem {
            name: "Plan mode proposal".to_string(),
            description: Some(limit_display_text(plan_text)),
            is_disabled: true,
            ..Default::default()
        });
    }
    items.extend(page.items.iter().cloned().map(work_item));
    if let Some(cursor) = next_cursor {
        let next_cursor = cursor.clone();
        items.push(SelectionItem {
            name: "Read next page".to_string(),
            description: Some(format!(
                "Continue reading {} from cursor {cursor}.",
                section_label(section)
            )),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::ReadSupervisorWork {
                    id,
                    section,
                    cursor: Some(next_cursor.clone()),
                });
            })],
            dismiss_on_select: false,
            ..Default::default()
        });
    }
    if page.items.is_empty() {
        items.push(SelectionItem {
            name: "No items in this section".to_string(),
            is_disabled: true,
            ..Default::default()
        });
    }

    SelectionViewParams {
        view_id: Some(VIEW_ID),
        title: Some(format!("Process {}: {}", id, section_label(section))),
        subtitle: Some(format!(
            "{} item(s) in this page{}",
            page.items.len(),
            page.next_cursor
                .as_deref()
                .map_or(String::new(), |cursor| format!(" | next cursor: {cursor}"))
        )),
        items,
        ..Default::default()
    }
}

fn process_item(record: ProcessRecord, current_pid: u32) -> SelectionItem {
    let id = record.id;
    let is_current = record.pid == current_pid;
    SelectionItem {
        name: format!("{} PID {}", kind_label(record.kind), record.pid),
        description: Some(process_description(&record)),
        is_current,
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenSupervisorProcessDetails { id });
        })],
        dismiss_on_select: true,
        search_value: Some(format!(
            "{} {} {} {} {} {}",
            record.pid,
            kind_label(record.kind),
            status_label(record.status),
            activity_label(record.activity),
            mode_label(record.mode),
            record.cwd.display()
        )),
        ..Default::default()
    }
}

fn process_description(record: &ProcessRecord) -> String {
    let thread_id = record.thread_id.as_deref().unwrap_or("-");
    let summary = record.summary.as_deref().unwrap_or("-");
    let error = record.error.as_deref().unwrap_or("-");
    format!(
        "status: {} | activity: {} | mode: {} | summary: {} | error: {} | cwd: {} | thread: {} | observed: {}",
        status_label(record.status),
        activity_label(record.activity),
        mode_label(record.mode),
        summary,
        error,
        record.cwd.display(),
        thread_id,
        record.last_observed_at
    )
}

fn process_details_description(record: &ProcessRecord) -> String {
    format!("PID {} | {}", record.pid, process_description(record))
}

fn work_item(item: WorkItem) -> SelectionItem {
    let mut description = String::new();
    if let Some(id) = item.id {
        append_work_field(&mut description, "id", &id);
    }
    if let Some(status) = item.status {
        append_work_field(&mut description, "status", &status);
    }
    if !item.content.is_empty() {
        if !description.is_empty() && !description.ends_with('\n') {
            description.push('\n');
        }
        description.push_str(&item.content);
    }
    if let Some(input) = item.input {
        append_work_field(&mut description, "input", &input);
    }
    if let Some(output) = item.output {
        append_work_field(&mut description, "output", &output);
    }
    if let Some(created_at) = item.created_at {
        append_work_field(&mut description, "created_at", &created_at.to_string());
    }
    SelectionItem {
        name: format!("#{} {}", item.index, item.title),
        description: Some(limit_display_text(description)),
        is_disabled: true,
        ..Default::default()
    }
}

fn append_work_field(description: &mut String, name: &str, value: &str) {
    if !description.is_empty() && !description.ends_with('\n') {
        description.push('\n');
    }
    description.push_str(name);
    description.push_str(": ");
    description.push_str(value);
}

fn limit_display_text(mut text: String) -> String {
    const MAX_DISPLAY_BYTES: usize = 8 * 1024;
    if text.len() <= MAX_DISPLAY_BYTES {
        return text;
    }
    let mut end = MAX_DISPLAY_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n[content truncated for dashboard display]");
    text
}

fn back_to_dashboard_item() -> SelectionItem {
    SelectionItem {
        name: "Back to process list".to_string(),
        actions: vec![Box::new(|tx| {
            tx.send(AppEvent::OpenSupervisorDashboard);
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn back_to_process_item(id: Uuid) -> SelectionItem {
    SelectionItem {
        name: "Back to process details".to_string(),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenSupervisorProcessDetails { id });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn kind_label(kind: ProcessKind) -> &'static str {
    match kind {
        ProcessKind::Tui => "TUI",
        ProcessKind::Cli => "CLI",
    }
}

fn status_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Starting => "starting",
        ProcessStatus::Running => "running",
        ProcessStatus::Stopping => "stopping",
        ProcessStatus::Exited => "exited",
        ProcessStatus::Unresponsive => "unresponsive",
    }
}

fn activity_label(activity: codex_supervisor::ActivityStatus) -> &'static str {
    match activity {
        codex_supervisor::ActivityStatus::Idle => "idle",
        codex_supervisor::ActivityStatus::Thinking => "thinking",
        codex_supervisor::ActivityStatus::ExecutingTool => "executingTool",
        codex_supervisor::ActivityStatus::WaitingForUserInput => "waitingForUserInput",
        codex_supervisor::ActivityStatus::WaitingForApproval => "waitingForApproval",
        codex_supervisor::ActivityStatus::Error => "error",
    }
}

fn mode_label(mode: ProcessMode) -> &'static str {
    match mode {
        ProcessMode::Default => "default",
        ProcessMode::Plan => "plan",
        ProcessMode::Review => "review",
        ProcessMode::Mixed => "mixed",
    }
}

fn section_label(section: WorkSection) -> &'static str {
    match section {
        WorkSection::Prompt => "prompt",
        WorkSection::Plan => "plan",
        WorkSection::Messages => "messages",
        WorkSection::ToolCalls => "tool calls",
    }
}

#[cfg(test)]
#[path = "supervisor_dashboard_tests.rs"]
mod tests;
