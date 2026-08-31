use serde::Deserialize;
use serde::Serialize;
use std::ffi::OsString;
use std::path::PathBuf;
use uuid::Uuid;

pub const MAX_PAGE_LIMIT: u32 = 50;

/// 由 supervisor 管理的 Codex 进程类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessKind {
    Tui,
    Cli,
}

/// 进程生命周期状态。`Unresponsive` 表示一次按需查询未能连接到 worker，不能继续
/// 视为当前可控的运行中进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessStatus {
    Starting,
    Running,
    Stopping,
    Exited,
    Unresponsive,
}

/// 当前 worker 的业务活动状态。生命周期和活动是两条独立维度：进程可以仍然
/// `Running`，但正在思考、执行工具或等待用户作答。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum ActivityStatus {
    #[default]
    Idle,
    Thinking,
    ExecutingTool,
    WaitingForUserInput,
    WaitingForApproval,
    Error,
}

/// 当前会话模式独立于活动状态，避免把“Plan 模式下正在思考”压扁成一个枚举值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum ProcessMode {
    #[default]
    Default,
    Plan,
    Review,
    Mixed,
}

/// worker 通过受控读服务返回的轻量状态。完整工作内容不进入 supervisor 状态表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    pub thread_id: Option<String>,
    pub activity: ActivityStatus,
    pub mode: ProcessMode,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub updated_at: i64,
}

impl Default for WorkerStatus {
    fn default() -> Self {
        Self {
            thread_id: None,
            activity: ActivityStatus::Idle,
            mode: ProcessMode::Default,
            summary: None,
            error: None,
            updated_at: 0,
        }
    }
}

/// 查询 worker 内容时使用的分页分区。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkSection {
    Prompt,
    Plan,
    Messages,
    ToolCalls,
}

/// 一条计划步骤。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub step: String,
    pub status: String,
}

/// 一条会话消息。正文仍由 worker 持有，查询时按页返回。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkMessage {
    pub role: String,
    pub content: String,
    pub created_at: Option<i64>,
}

/// 一次工具调用及其输入输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    /// 工具生命周期的稳定 ID，用于把 started/completed 两个事件合并成一条记录。
    /// 该字段保持可选，以便新客户端读取旧 supervisor 返回的历史页面。
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub input: String,
    pub output: Option<String>,
    pub status: String,
    pub created_at: Option<i64>,
}

/// supervisor 向 worker 请求的一页工作内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkPage {
    pub process_id: Uuid,
    pub section: WorkSection,
    pub items: Vec<WorkItem>,
    pub next_cursor: Option<String>,
    /// Plan mode 生成的方案文本。只在 Plan 分区的第一页返回，checklist 仍在 items 中分页。
    #[serde(default)]
    pub plan_text: Option<String>,
}

/// 分页返回的统一条目。工具调用的 input/output 保持独立字段，Agent 不需要再解析
/// 一段人为拼接的文本才能判断工具是否失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub index: usize,
    pub title: String,
    pub content: String,
    /// 工具调用 ID；普通消息和计划项没有该字段。
    #[serde(default)]
    pub id: Option<String>,
    pub status: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub created_at: Option<i64>,
}

/// worker 维护的完整工作内容。supervisor 只通过受控查询读取它的分页结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerDetails {
    pub prompt: Option<String>,
    /// Plan mode 的模型方案文本，与 update_plan checklist 分开保存。
    #[serde(default)]
    pub plan_text: Option<String>,
    pub plan: Vec<PlanStep>,
    pub messages: Vec<WorkMessage>,
    pub tool_calls: Vec<ToolCallRecord>,
}

impl WorkerDetails {
    pub(crate) fn page(
        &self,
        process_id: Uuid,
        section: WorkSection,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<WorkPage, String> {
        let offset = cursor.map_or(Ok(0), |cursor| {
            cursor
                .parse::<usize>()
                .map_err(|_| format!("invalid work page cursor `{cursor}`"))
        })?;
        let limit = limit.clamp(1, MAX_PAGE_LIMIT) as usize;
        let items = match section {
            WorkSection::Prompt => self
                .prompt
                .as_ref()
                .map(|prompt| {
                    vec![WorkItem {
                        index: 0,
                        title: "prompt".to_string(),
                        content: prompt.clone(),
                        id: None,
                        status: None,
                        input: None,
                        output: None,
                        created_at: None,
                    }]
                })
                .unwrap_or_default(),
            WorkSection::Plan => self
                .plan
                .iter()
                .enumerate()
                .map(|(index, step)| WorkItem {
                    index,
                    title: "plan step".to_string(),
                    content: step.step.clone(),
                    id: None,
                    status: Some(step.status.clone()),
                    input: None,
                    output: None,
                    created_at: None,
                })
                .collect(),
            WorkSection::Messages => self
                .messages
                .iter()
                .enumerate()
                .map(|(index, message)| WorkItem {
                    index,
                    title: message.role.clone(),
                    content: message.content.clone(),
                    id: None,
                    status: None,
                    input: None,
                    output: None,
                    created_at: message.created_at,
                })
                .collect(),
            WorkSection::ToolCalls => self
                .tool_calls
                .iter()
                .enumerate()
                .map(|(index, call)| WorkItem {
                    index,
                    title: call.name.clone(),
                    content: String::new(),
                    id: call.id.clone(),
                    status: Some(call.status.clone()),
                    input: Some(call.input.clone()),
                    output: call.output.clone(),
                    created_at: call.created_at,
                })
                .collect(),
        };
        if offset > items.len() {
            return Err(format!(
                "work page cursor {offset} is past the end of {} items",
                items.len()
            ));
        }
        // 页大小同时受条数和 IPC frame 大小约束。单条内容虽然已经截断到有界长度，
        // 但 50 条大工具调用仍可能让序列化后的响应超过 1 MiB；按最终 wire payload
        // 逐条试装，保证 Agent 和 TUI 得到的每一页都能真正通过 supervisor 传输。
        let mut page_items = Vec::new();
        let plan_text = (section == WorkSection::Plan && offset == 0)
            .then(|| self.plan_text.clone())
            .flatten();
        for item in items.iter().skip(offset).take(limit) {
            page_items.push(item.clone());
            let candidate_end = offset + page_items.len();
            let candidate_page = WorkPage {
                process_id,
                section,
                items: page_items.clone(),
                next_cursor: (candidate_end < items.len()).then(|| candidate_end.to_string()),
                plan_text: plan_text.clone(),
            };
            let candidate_size =
                serde_json::to_vec(&crate::protocol::Response::WorkPage(candidate_page))
                    .map_err(|error| format!("failed to size work page: {error}"))?
                    .len();
            if candidate_size > crate::MAX_FRAME_SIZE {
                page_items.pop();
                if page_items.is_empty() {
                    return Err(format!(
                        "work item at index {offset} exceeds supervisor frame limit"
                    ));
                }
                break;
            }
        }
        let end = offset + page_items.len();
        let next_cursor = (end < items.len()).then(|| end.to_string());
        Ok(WorkPage {
            process_id,
            section,
            items: page_items,
            next_cursor,
            plan_text,
        })
    }
}

/// dashboard 展示的单条进程事实记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRecord {
    pub id: Uuid,
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub kind: ProcessKind,
    pub status: ProcessStatus,
    pub activity: ActivityStatus,
    pub mode: ProcessMode,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub thread_id: Option<String>,
    pub created_at: i64,
    pub last_observed_at: i64,
    pub last_state_update_at: i64,
    pub exit_code: Option<i32>,
}

/// supervisor 一次查询返回的快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorSnapshot {
    pub protocol_version: u32,
    pub daemon_pid: u32,
    pub processes: Vec<ProcessRecord>,
}

/// worker 启动参数。参数由官方 CLI 入口提供，supervisor 不解析业务命令行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub kind: ProcessKind,
    pub thread_id: Option<String>,
}
