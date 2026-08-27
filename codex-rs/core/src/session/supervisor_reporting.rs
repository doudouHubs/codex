//! 将 core 的统一事件流投影到本地 supervisor 控制面。
//!
//! TUI、`codex exec` 和 app-server 内嵌会话最终都会经过 [`Session::send_event`]，
//! 因此这里是跨前端共享状态的唯一事实边界。投影只更新有界内存快照，不参与事件
//! 投递、rollout 持久化或模型控制，supervisor 不可用时也不会影响主流程。

use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::items::TurnItem;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HasLegacyEvent;
use codex_supervisor::ActivityStatus;
use codex_supervisor::PlanStep;
use codex_supervisor::ProcessMode;
use codex_supervisor::SupervisorReporter;
use codex_supervisor::ToolCallRecord;
use codex_supervisor::WorkMessage;

/// 事件投影依赖的最小写入边界。生产环境实现连接内存 reporter，测试实现可以在不
/// 触碰全局 OnceLock 的情况下验证 canonical 事件到 dashboard 数据的映射。
trait SupervisorEventSink {
    fn set_thread_id(&self, thread_id: Option<String>);
    fn set_activity(&self, activity: ActivityStatus, mode: ProcessMode, summary: Option<String>);
    fn set_error(&self, error: String);
    fn set_prompt(&self, prompt: String);
    fn set_plan_text(&self, plan_text: String);
    fn append_plan_delta(&self, delta: String);
    fn set_plan(&self, plan: Vec<PlanStep>);
    fn append_message(&self, role: String, content: String);
    fn upsert_tool_call(
        &self,
        id: String,
        name: String,
        input: String,
        output: Option<String>,
        status: String,
    );
}

impl SupervisorEventSink for SupervisorReporter {
    fn set_thread_id(&self, thread_id: Option<String>) {
        self.set_thread_id(thread_id);
    }

    fn set_activity(&self, activity: ActivityStatus, mode: ProcessMode, summary: Option<String>) {
        self.set_activity(activity, mode, summary);
    }

    fn set_error(&self, error: String) {
        self.set_error(error);
    }

    fn set_prompt(&self, prompt: String) {
        self.set_prompt(prompt);
    }

    fn set_plan_text(&self, plan_text: String) {
        self.set_plan_text(plan_text);
    }

    fn append_plan_delta(&self, delta: String) {
        self.append_plan_delta(delta);
    }

    fn set_plan(&self, plan: Vec<PlanStep>) {
        self.set_plan(plan);
    }

    fn append_message(&self, role: String, content: String) {
        self.append_message(WorkMessage {
            role,
            content,
            created_at: None,
        });
    }

    fn upsert_tool_call(
        &self,
        id: String,
        name: String,
        input: String,
        output: Option<String>,
        status: String,
    ) {
        self.upsert_tool_call(ToolCallRecord {
            id: Some(id),
            name,
            input,
            output,
            status,
            created_at: None,
        });
    }
}

pub(crate) fn record_event(thread_id: ThreadId, mode: ModeKind, event: &EventMsg) {
    let Some(reporter) = codex_supervisor::current_reporter() else {
        return;
    };

    record_event_with_sink(&reporter, thread_id, mode, event);
}

fn record_event_with_sink<S: SupervisorEventSink>(
    sink: &S,
    thread_id: ThreadId,
    mode: ModeKind,
    event: &EventMsg,
) {
    sink.set_thread_id(Some(thread_id.to_string()));
    let process_mode = map_process_mode(mode);
    match event {
        EventMsg::ItemStarted(item) => {
            match &item.item {
                TurnItem::UserMessage(message) => {
                    let prompt = user_prompt(message);
                    sink.set_prompt(prompt.clone());
                    sink.append_message("user".to_string(), prompt);
                    sink.set_activity(
                        ActivityStatus::Thinking,
                        process_mode,
                        Some("processing user input".to_string()),
                    );
                }
                TurnItem::AgentMessage(_) => {
                    sink.set_activity(
                        ActivityStatus::Thinking,
                        process_mode,
                        Some("generating response".to_string()),
                    );
                }
                TurnItem::Plan(_) => {
                    // 新一轮 Plan mode 开始时先清掉上一轮方案，避免 dashboard 在流式
                    // 输出期间把旧方案误报成当前方案。
                    sink.set_plan_text(String::new());
                    sink.set_activity(
                        ActivityStatus::Thinking,
                        ProcessMode::Plan,
                        Some("generating plan".to_string()),
                    );
                }
                _ => {}
            }
            if !matches!(
                &item.item,
                TurnItem::UserMessage(_) | TurnItem::AgentMessage(_) | TurnItem::Plan(_)
            ) {
                for legacy_event in item.as_legacy_events(false) {
                    record_event_with_sink(sink, thread_id, mode, &legacy_event);
                }
            }
        }
        EventMsg::ItemCompleted(item) => {
            match &item.item {
                TurnItem::UserMessage(message) => {
                    // started 事件已经记录了消息正文；completed 只再次校准 prompt，
                    // 避免同一个 canonical item 在两个生命周期事件中产生重复消息。
                    sink.set_prompt(user_prompt(message));
                }
                TurnItem::AgentMessage(message) => {
                    sink.append_message("assistant".to_string(), agent_message_text(message));
                    sink.set_activity(
                        ActivityStatus::Thinking,
                        process_mode,
                        Some("generating response".to_string()),
                    );
                }
                TurnItem::Plan(plan) => {
                    // Plan item 的最终文本是 canonical 事件中最完整的版本；它覆盖流式
                    // delta，防止 provider 在结束时做了规范化而留下半截方案。
                    sink.set_plan_text(plan.text.clone());
                }
                _ => {
                    for legacy_event in item.as_legacy_events(false) {
                        record_event_with_sink(sink, thread_id, mode, &legacy_event);
                    }
                }
            }
        }
        EventMsg::PlanDelta(plan) => {
            sink.append_plan_delta(plan.delta.clone());
            sink.set_activity(
                ActivityStatus::Thinking,
                ProcessMode::Plan,
                Some("streaming plan".to_string()),
            );
        }
        EventMsg::TurnStarted(event) => {
            sink.set_activity(
                ActivityStatus::Thinking,
                map_process_mode(event.collaboration_mode_kind),
                Some("turn started".to_string()),
            );
        }
        EventMsg::TurnComplete(event) => {
            if let Some(error) = &event.error {
                sink.set_error(error.message.clone());
            } else {
                sink.set_activity(
                    ActivityStatus::Idle,
                    process_mode,
                    Some("turn completed".to_string()),
                );
            }
        }
        EventMsg::TurnAborted(event) => {
            sink.set_activity(
                ActivityStatus::Idle,
                process_mode,
                Some(format!("turn aborted: {reason:?}", reason = event.reason)),
            );
        }
        EventMsg::Error(error) => sink.set_error(error.message.clone()),
        // Session::send_event 已经在 canonical item 后发送 legacy UserMessage；如果这里
        // 再记录正文，同一轮输入会出现两次。dashboard 的消息事实只来自 TurnItem，
        // legacy 分支保留为空是为了明确禁止旧事件覆盖 canonical 数据。
        EventMsg::UserMessage(_) | EventMsg::AgentMessage(_) => {}
        EventMsg::AgentReasoning(reasoning) => {
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some(bound_summary(&reasoning.text)),
            );
        }
        EventMsg::AgentReasoningRawContent(reasoning) => {
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some(bound_summary(&reasoning.text)),
            );
        }
        EventMsg::AgentMessageContentDelta(_) | EventMsg::ReasoningContentDelta(_) => {
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("streaming model output".to_string()),
            );
        }
        EventMsg::PlanUpdate(plan) => {
            sink.set_plan(
                plan.plan
                    .iter()
                    .map(|step| codex_supervisor::PlanStep {
                        step: step.step.clone(),
                        status: step_status(step.status.clone()),
                    })
                    .collect(),
            );
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                plan.explanation
                    .clone()
                    .or_else(|| Some("plan updated".to_string())),
            );
        }
        EventMsg::EnteredReviewMode(event) => {
            sink.set_activity(
                ActivityStatus::Thinking,
                ProcessMode::Review,
                event
                    .user_facing_hint
                    .clone()
                    .or_else(|| Some("review mode".to_string())),
            );
        }
        EventMsg::ExitedReviewMode(_) => {
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("left review mode".to_string()),
            );
        }
        EventMsg::ExecCommandBegin(command) => {
            let summary = command.command.join(" ");
            record_tool_call_with_sink(
                sink,
                command.call_id.clone(),
                "exec".to_string(),
                serde_json::json!({
                    "callId": command.call_id,
                    "command": command.command,
                    "cwd": command.cwd,
                })
                .to_string(),
                None,
                "running".to_string(),
            );
            sink.set_activity(ActivityStatus::ExecutingTool, process_mode, Some(summary));
        }
        EventMsg::ExecCommandEnd(command) => {
            record_tool_call_with_sink(
                sink,
                command.call_id.clone(),
                "exec".to_string(),
                serde_json::json!({
                    "callId": command.call_id,
                    "command": command.command,
                })
                .to_string(),
                Some(command.formatted_output.clone()),
                format!("{status:?}", status = command.status).to_lowercase(),
            );
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("tool completed".to_string()),
            );
        }
        EventMsg::McpToolCallBegin(tool) => {
            record_tool_call_with_sink(
                sink,
                tool.call_id.clone(),
                format!(
                    "mcp:{server}:{tool}",
                    server = tool.invocation.server,
                    tool = tool.invocation.tool
                ),
                serde_json::json!({
                    "callId": tool.call_id,
                    "arguments": tool.invocation.arguments,
                })
                .to_string(),
                None,
                "running".to_string(),
            );
            sink.set_activity(
                ActivityStatus::ExecutingTool,
                process_mode,
                Some(format!(
                    "MCP tool {server}:{tool}",
                    server = tool.invocation.server,
                    tool = tool.invocation.tool
                )),
            );
        }
        EventMsg::McpToolCallEnd(tool) => {
            let (output, status) = match &tool.result {
                Ok(result) => (json_string(result), "completed".to_string()),
                Err(error) => (Some(error.clone()), "failed".to_string()),
            };
            record_tool_call_with_sink(
                sink,
                tool.call_id.clone(),
                format!(
                    "mcp:{server}:{tool}",
                    server = tool.invocation.server,
                    tool = tool.invocation.tool
                ),
                serde_json::json!({
                    "callId": tool.call_id,
                    "arguments": tool.invocation.arguments,
                })
                .to_string(),
                output,
                status,
            );
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("MCP tool completed".to_string()),
            );
        }
        EventMsg::PatchApplyBegin(patch) => {
            record_tool_call_with_sink(
                sink,
                patch.call_id.clone(),
                "apply_patch".to_string(),
                serde_json::json!({
                    "callId": patch.call_id,
                    "changes": patch.changes,
                })
                .to_string(),
                None,
                "running".to_string(),
            );
            sink.set_activity(
                ActivityStatus::ExecutingTool,
                process_mode,
                Some("applying patch".to_string()),
            );
        }
        EventMsg::PatchApplyEnd(patch) => {
            record_tool_call_with_sink(
                sink,
                patch.call_id.clone(),
                "apply_patch".to_string(),
                serde_json::json!({ "callId": patch.call_id }).to_string(),
                Some(if patch.stderr.is_empty() {
                    patch.stdout.clone()
                } else {
                    patch.stderr.clone()
                }),
                format!("{status:?}", status = patch.status).to_lowercase(),
            );
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("patch completed".to_string()),
            );
        }
        EventMsg::DynamicToolCallRequest(tool) => {
            record_tool_call_with_sink(
                sink,
                tool.call_id.clone(),
                tool.tool.clone(),
                serde_json::json!({
                    "callId": tool.call_id,
                    "namespace": tool.namespace,
                    "arguments": tool.arguments,
                })
                .to_string(),
                None,
                "running".to_string(),
            );
            sink.set_activity(
                ActivityStatus::ExecutingTool,
                process_mode,
                Some(format!("dynamic tool {name}", name = tool.tool)),
            );
        }
        EventMsg::DynamicToolCallResponse(tool) => {
            record_tool_call_with_sink(
                sink,
                tool.call_id.clone(),
                tool.tool.clone(),
                serde_json::json!({ "callId": tool.call_id }).to_string(),
                json_string(&tool.content_items),
                if tool.success {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                },
            );
            sink.set_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("dynamic tool completed".to_string()),
            );
        }
        EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::GuardianAssessment(_) => sink.set_activity(
            ActivityStatus::WaitingForApproval,
            process_mode,
            Some("waiting for approval".to_string()),
        ),
        EventMsg::RequestUserInput(_) => sink.set_activity(
            ActivityStatus::WaitingForUserInput,
            process_mode,
            Some("waiting for user input".to_string()),
        ),
        // 流错误可能随后自动重试，但在当前时刻仍然是 Agent 可观测的失败状态；
        // 不能把错误文本挂在 Thinking 上，否则 dashboard 会误报进程仍在正常工作。
        EventMsg::StreamError(error) => sink.set_error(error.message.clone()),
        EventMsg::ShutdownComplete => sink.set_activity(
            ActivityStatus::Idle,
            process_mode,
            Some("shutdown complete".to_string()),
        ),
        _ => {}
    }
}

fn record_tool_call_with_sink<S: SupervisorEventSink>(
    sink: &S,
    id: String,
    name: String,
    input: String,
    output: Option<String>,
    status: String,
) {
    sink.upsert_tool_call(id, name, input, output, status);
}

fn map_process_mode(mode: ModeKind) -> ProcessMode {
    match mode {
        ModeKind::Plan => ProcessMode::Plan,
        ModeKind::Default => ProcessMode::Default,
    }
}

fn user_prompt(message: &codex_protocol::items::UserMessageItem) -> String {
    let legacy = message.as_legacy_user_message_event();
    codex_protocol::protocol::user_message_preview(&legacy).unwrap_or_default()
}

fn agent_message_text(message: &codex_protocol::items::AgentMessageItem) -> String {
    message
        .content
        .iter()
        .map(|content| match content {
            codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

fn step_status(status: StepStatus) -> String {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::InProgress => "inProgress",
        StepStatus::Completed => "completed",
    }
    .to_string()
}

fn json_string<T: serde::Serialize>(value: &T) -> Option<String> {
    serde_json::to_string(value).ok()
}

fn bound_summary(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "thinking".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
#[path = "supervisor_reporting_tests.rs"]
mod tests;
