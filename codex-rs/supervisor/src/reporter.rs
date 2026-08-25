use crate::ActivityStatus;
use crate::PlanStep;
use crate::ProcessMode;
use crate::ToolCallRecord;
use crate::WorkMessage;
use crate::WorkerDetails;
use crate::WorkerStatus;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;

const MAX_WORK_ITEMS: usize = 256;
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// 当前 worker 的状态和内容写入句柄。它不负责发送 IPC，心跳和受控读服务会读取同一份
/// 内存快照，因此状态上报不会阻塞核心 Agent 逻辑。
#[derive(Clone, Debug)]
pub struct SupervisorReporter {
    state: Arc<RwLock<ReporterState>>,
}

#[derive(Debug, Default)]
struct ReporterState {
    status: WorkerStatus,
    details: WorkerDetails,
}

impl SupervisorReporter {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ReporterState::default())),
        }
    }

    pub fn update_status(&self, mut status: WorkerStatus) {
        status.thread_id = status.thread_id.map(|thread_id| bound_text(&thread_id));
        status.summary = status.summary.map(|summary| bound_text(&summary));
        status.error = status.error.map(|error| bound_text(&error));
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.status = status;
    }

    pub fn set_thread_id(&self, thread_id: Option<String>) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.status.thread_id = thread_id.map(|thread_id| bound_text(&thread_id));
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn set_activity(
        &self,
        activity: ActivityStatus,
        mode: ProcessMode,
        summary: Option<String>,
    ) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.status.activity = activity;
        state.status.mode = mode;
        state.status.summary = summary.map(|summary| bound_text(&summary));
        state.status.error = None;
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn set_error(&self, error: String) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.status.activity = ActivityStatus::Error;
        state.status.error = Some(bound_text(&error));
        state.status.summary = Some(bound_text(&error));
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn set_prompt(&self, prompt: String) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.details.prompt = Some(bound_text(&prompt));
    }

    pub fn set_plan(&self, plan: Vec<PlanStep>) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.details.plan = plan
            .into_iter()
            .take(MAX_WORK_ITEMS)
            .map(|step| PlanStep {
                step: bound_text(&step.step),
                status: bound_text(&step.status),
            })
            .collect();
    }

    pub fn append_message(&self, mut message: WorkMessage) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        message.role = bound_text(&message.role);
        message.content = bound_text(&message.content);
        push_bounded(&mut state.details.messages, message);
    }

    pub fn append_tool_call(&self, mut call: ToolCallRecord) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        call.name = bound_text(&call.name);
        call.input = bound_text(&call.input);
        call.output = call.output.map(|output| bound_text(&output));
        call.status = bound_text(&call.status);
        push_bounded(&mut state.details.tool_calls, call);
    }

    pub(crate) fn status(&self) -> WorkerStatus {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status
            .clone()
    }

    pub(crate) fn page(
        &self,
        process_id: uuid::Uuid,
        section: crate::WorkSection,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<crate::WorkPage, String> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .details
            .page(process_id, section, cursor, limit)
    }
}

fn push_bounded<T>(items: &mut Vec<T>, item: T) {
    if items.len() >= MAX_WORK_ITEMS {
        items.remove(0);
    }
    items.push(item);
}

fn bound_text(value: &str) -> String {
    if value.len() <= MAX_TEXT_BYTES {
        return value.to_string();
    }
    let mut end = MAX_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[content truncated]", &value[..end])
}

static CURRENT_REPORTER: OnceLock<SupervisorReporter> = OnceLock::new();

pub(crate) fn install_current_reporter(reporter: SupervisorReporter) {
    let _ = CURRENT_REPORTER.set(reporter);
}

pub fn current_reporter() -> Option<SupervisorReporter> {
    CURRENT_REPORTER.get().cloned()
}

pub fn report_current_thread_id(thread_id: Option<String>) {
    if let Some(reporter) = current_reporter() {
        reporter.set_thread_id(thread_id);
    }
}

pub fn report_current_activity(
    activity: ActivityStatus,
    mode: ProcessMode,
    summary: Option<String>,
) {
    if let Some(reporter) = current_reporter() {
        reporter.set_activity(activity, mode, summary);
    }
}

pub fn report_current_error(error: String) {
    if let Some(reporter) = current_reporter() {
        reporter.set_error(error);
    }
}

pub fn record_current_prompt(prompt: String) {
    if let Some(reporter) = current_reporter() {
        reporter.set_prompt(prompt);
    }
}

pub fn record_current_plan(plan: Vec<PlanStep>) {
    if let Some(reporter) = current_reporter() {
        reporter.set_plan(plan);
    }
}

pub fn record_current_message(role: String, content: String) {
    if let Some(reporter) = current_reporter() {
        reporter.append_message(WorkMessage {
            role,
            content,
            created_at: Some(crate::unix_seconds()),
        });
    }
}

pub fn record_current_tool_call(
    name: String,
    input: String,
    output: Option<String>,
    status: String,
) {
    if let Some(reporter) = current_reporter() {
        reporter.append_tool_call(ToolCallRecord {
            name,
            input,
            output,
            status,
            created_at: Some(crate::unix_seconds()),
        });
    }
}
