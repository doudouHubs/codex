//! 将 core 的统一事件流投影到本地 supervisor 控制面。
//!
//! TUI、`codex exec` 和 app-server 内嵌会话最终都会经过 [`Session::send_event`]，
//! 因此这里是跨前端共享状态的唯一事实边界。投影只更新有界内存快照，不参与事件
//! 投递、rollout 持久化或模型控制，supervisor 不可用时也不会影响主流程。

use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::protocol::EventMsg;
use codex_supervisor::ActivityStatus;
use codex_supervisor::ProcessMode;
use codex_supervisor::record_current_message;
use codex_supervisor::record_current_plan;
use codex_supervisor::record_current_prompt;
use codex_supervisor::record_current_tool_call;
use codex_supervisor::report_current_activity;
use codex_supervisor::report_current_error;
use codex_supervisor::report_current_thread_id;
pub(crate) fn record_event(thread_id: ThreadId, mode: ModeKind, event: &EventMsg) {
    if codex_supervisor::current_reporter().is_none() {
        return;
    }

    report_current_thread_id(Some(thread_id.to_string()));
    let process_mode = map_process_mode(mode);
    match event {
        EventMsg::TurnStarted(event) => {
            report_current_activity(
                ActivityStatus::Thinking,
                map_process_mode(event.collaboration_mode_kind),
                Some("turn started".to_string()),
            );
        }
        EventMsg::TurnComplete(event) => {
            if let Some(error) = &event.error {
                report_current_error(error.message.clone());
            } else {
                report_current_activity(
                    ActivityStatus::Idle,
                    process_mode,
                    Some("turn completed".to_string()),
                );
            }
        }
        EventMsg::TurnAborted(event) => {
            report_current_activity(
                ActivityStatus::Idle,
                process_mode,
                Some(format!("turn aborted: {:?}", event.reason)),
            );
        }
        EventMsg::Error(error) => report_current_error(error.message.clone()),
        EventMsg::UserMessage(message) => {
            record_current_prompt(message.message.clone());
            record_current_message("user".to_string(), message.message.clone());
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("processing user input".to_string()),
            );
        }
        EventMsg::AgentMessage(message) => {
            record_current_message("assistant".to_string(), message.message.clone());
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("generating response".to_string()),
            );
        }
        EventMsg::AgentReasoning(reasoning) => {
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some(bound_summary(&reasoning.text)),
            );
        }
        EventMsg::AgentReasoningRawContent(reasoning) => {
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some(bound_summary(&reasoning.text)),
            );
        }
        EventMsg::AgentMessageContentDelta(_) | EventMsg::ReasoningContentDelta(_) => {
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("streaming model output".to_string()),
            );
        }
        EventMsg::PlanUpdate(plan) => {
            record_current_plan(
                plan.plan
                    .iter()
                    .map(|step| codex_supervisor::PlanStep {
                        step: step.step.clone(),
                        status: step_status(step.status.clone()),
                    })
                    .collect(),
            );
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                plan.explanation
                    .clone()
                    .or_else(|| Some("plan updated".to_string())),
            );
        }
        EventMsg::EnteredReviewMode(event) => {
            report_current_activity(
                ActivityStatus::Thinking,
                ProcessMode::Review,
                event
                    .user_facing_hint
                    .clone()
                    .or_else(|| Some("review mode".to_string())),
            );
        }
        EventMsg::ExitedReviewMode(_) => {
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("left review mode".to_string()),
            );
        }
        EventMsg::ExecCommandBegin(command) => {
            let summary = command.command.join(" ");
            record_current_tool_call(
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
            report_current_activity(ActivityStatus::ExecutingTool, process_mode, Some(summary));
        }
        EventMsg::ExecCommandEnd(command) => {
            record_current_tool_call(
                "exec".to_string(),
                serde_json::json!({
                    "callId": command.call_id,
                    "command": command.command,
                })
                .to_string(),
                Some(command.formatted_output.clone()),
                format!("{:?}", command.status).to_lowercase(),
            );
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("tool completed".to_string()),
            );
        }
        EventMsg::McpToolCallBegin(tool) => {
            record_current_tool_call(
                format!("mcp:{}:{}", tool.invocation.server, tool.invocation.tool),
                serde_json::json!({
                    "callId": tool.call_id,
                    "arguments": tool.invocation.arguments,
                })
                .to_string(),
                None,
                "running".to_string(),
            );
            report_current_activity(
                ActivityStatus::ExecutingTool,
                process_mode,
                Some(format!(
                    "MCP tool {}:{}",
                    tool.invocation.server, tool.invocation.tool
                )),
            );
        }
        EventMsg::McpToolCallEnd(tool) => {
            let (output, status) = match &tool.result {
                Ok(result) => (json_string(result), "completed".to_string()),
                Err(error) => (Some(error.clone()), "failed".to_string()),
            };
            record_current_tool_call(
                format!("mcp:{}:{}", tool.invocation.server, tool.invocation.tool),
                serde_json::json!({
                    "callId": tool.call_id,
                    "arguments": tool.invocation.arguments,
                })
                .to_string(),
                output,
                status,
            );
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("MCP tool completed".to_string()),
            );
        }
        EventMsg::PatchApplyBegin(patch) => {
            record_current_tool_call(
                "apply_patch".to_string(),
                serde_json::json!({
                    "callId": patch.call_id,
                    "changes": patch.changes,
                })
                .to_string(),
                None,
                "running".to_string(),
            );
            report_current_activity(
                ActivityStatus::ExecutingTool,
                process_mode,
                Some("applying patch".to_string()),
            );
        }
        EventMsg::PatchApplyEnd(patch) => {
            record_current_tool_call(
                "apply_patch".to_string(),
                serde_json::json!({ "callId": patch.call_id }).to_string(),
                Some(if patch.stderr.is_empty() {
                    patch.stdout.clone()
                } else {
                    patch.stderr.clone()
                }),
                format!("{:?}", patch.status).to_lowercase(),
            );
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("patch completed".to_string()),
            );
        }
        EventMsg::DynamicToolCallRequest(tool) => {
            record_current_tool_call(
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
            report_current_activity(
                ActivityStatus::ExecutingTool,
                process_mode,
                Some(format!("dynamic tool {}", tool.tool)),
            );
        }
        EventMsg::DynamicToolCallResponse(tool) => {
            record_current_tool_call(
                tool.tool.clone(),
                serde_json::json!({ "callId": tool.call_id }).to_string(),
                json_string(&tool.content_items),
                if tool.success {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                },
            );
            report_current_activity(
                ActivityStatus::Thinking,
                process_mode,
                Some("dynamic tool completed".to_string()),
            );
        }
        EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::GuardianAssessment(_) => report_current_activity(
            ActivityStatus::WaitingForApproval,
            process_mode,
            Some("waiting for approval".to_string()),
        ),
        EventMsg::RequestUserInput(_) => report_current_activity(
            ActivityStatus::WaitingForUserInput,
            process_mode,
            Some("waiting for user input".to_string()),
        ),
        // 流错误可能随后自动重试，但在当前时刻仍然是 Agent 可观测的失败状态；
        // 不能把错误文本挂在 Thinking 上，否则 dashboard 会误报进程仍在正常工作。
        EventMsg::StreamError(error) => report_current_error(error.message.clone()),
        EventMsg::ShutdownComplete => report_current_activity(
            ActivityStatus::Idle,
            process_mode,
            Some("shutdown complete".to_string()),
        ),
        _ => {}
    }
}

fn map_process_mode(mode: ModeKind) -> ProcessMode {
    match mode {
        ModeKind::Plan => ProcessMode::Plan,
        ModeKind::Default => ProcessMode::Default,
    }
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
