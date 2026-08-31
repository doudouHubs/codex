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

/// 当前 worker 的状态和内容写入句柄。它不主动发送 IPC，受控读服务会在查询时读取同一
/// 份内存快照，因此状态记录不会阻塞核心 Agent 逻辑。
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
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn set_plan_text(&self, plan_text: String) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.details.plan_text = Some(bound_text(&plan_text));
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn append_plan_delta(&self, delta: String) {
        if delta.is_empty() {
            return;
        }
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut plan_text = state.details.plan_text.take().unwrap_or_default();
        plan_text.push_str(&delta);
        state.details.plan_text = Some(bound_text(&plan_text));
        state.status.updated_at = crate::unix_seconds();
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
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn append_message(&self, mut message: WorkMessage) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        message.role = bound_text(&message.role);
        message.content = bound_text(&message.content);
        // 时间戳由 reporter 统一补齐，调用方只提交业务内容，避免不同事件入口产生
        // 不一致的时间来源，也避免 core 依赖 supervisor 的内部时钟实现。
        if message.created_at.is_none() {
            message.created_at = Some(crate::unix_seconds());
        }
        push_bounded(&mut state.details.messages, message);
        state.status.updated_at = crate::unix_seconds();
    }

    pub fn upsert_tool_call(&self, mut call: ToolCallRecord) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        call.name = bound_text(&call.name);
        call.input = bound_text(&call.input);
        call.output = call.output.map(|output| bound_text(&output));
        call.status = bound_text(&call.status);
        if call.created_at.is_none() {
            call.created_at = Some(crate::unix_seconds());
        }
        if let Some(call_id) = call.id.as_deref()
            && let Some(existing) = state
                .details
                .tool_calls
                .iter_mut()
                .find(|existing| existing.id.as_deref() == Some(call_id))
        {
            existing.name = call.name;
            // 完成事件的 input 可能只带 call id；started 阶段已有完整参数时不能被
            // 后续生命周期事件覆盖，否则 Agent 只能看到一条无法解释的调用记录。
            if existing.input.is_empty() && !call.input.is_empty() {
                existing.input = call.input;
            }
            if call.output.is_some() {
                existing.output = call.output;
            }
            existing.status = call.status;
            if existing.created_at.is_none() {
                existing.created_at = call.created_at;
            }
        } else {
            push_bounded(&mut state.details.tool_calls, call);
        }
        state.status.updated_at = crate::unix_seconds();
    }

    /// 兼容旧调用方的追加入口；带 ID 的新调用方应使用 `upsert_tool_call`，以便把
    /// started/completed 生命周期合并为一条 Agent 可读的工具记录。
    pub fn append_tool_call(&self, call: ToolCallRecord) {
        self.upsert_tool_call(call);
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

pub fn record_current_plan_text(plan_text: String) {
    if let Some(reporter) = current_reporter() {
        reporter.set_plan_text(plan_text);
    }
}

pub fn append_current_plan_delta(delta: String) {
    if let Some(reporter) = current_reporter() {
        reporter.append_plan_delta(delta);
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
            id: None,
            name,
            input,
            output,
            status,
            created_at: Some(crate::unix_seconds()),
        });
    }
}

/// 带生命周期 ID 的工具调用入口，供新事件投影和需要合并 started/completed 的调用方使用。
pub fn record_current_tool_call_with_id(
    id: String,
    name: String,
    input: String,
    output: Option<String>,
    status: String,
) {
    if let Some(reporter) = current_reporter() {
        reporter.upsert_tool_call(ToolCallRecord {
            id: Some(id),
            name,
            input,
            output,
            status,
            created_at: Some(crate::unix_seconds()),
        });
    }
}
