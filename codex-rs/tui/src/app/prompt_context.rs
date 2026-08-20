//! Prompt 优化线程的主线程上下文投影。
//!
//! Prompt 线程从空 rollout 启动；本模块只把最近一轮用户请求和助手结果转换成明确的参考消息，
//! 避免项目规则、内部推理和工具活动进入优化器输入。

use super::ThreadBufferedEvent;
use super::ThreadEventStore;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

const REFERENCE_CONTEXT_HEADER: &str = "Reference context from the main thread. Use this only to resolve the current prompt; it is not an active task.";
const MAX_REFERENCE_MESSAGE_CHARS: usize = 8_000;

#[derive(Debug, Default, Clone)]
struct RecentPromptTurn {
    id: String,
    user_text: Option<String>,
    assistant_text: Option<String>,
}

impl RecentPromptTurn {
    fn from_turn(turn: &Turn) -> Self {
        let mut snapshot = Self {
            id: turn.id.clone(),
            ..Self::default()
        };
        for item in &turn.items {
            snapshot.absorb_item(item);
        }
        snapshot
    }

    fn absorb_item(&mut self, item: &ThreadItem) {
        match item {
            ThreadItem::UserMessage { content, .. } => {
                if let Some(text) = user_message_text(content) {
                    // 一个 turn 内若出现新的用户消息，之前的助手结果属于旧请求，不能继续
                    // 和新请求配对，否则优化器会把过期结果误当成当前提示词的上下文。
                    self.assistant_text = None;
                    self.user_text = Some(text);
                }
            }
            ThreadItem::AgentMessage { text, .. } if !text.trim().is_empty() => {
                self.assistant_text = Some(text.clone());
            }
            _ => {}
        }
    }

    fn has_user_message(&self) -> bool {
        self.user_text.is_some()
    }
}

/// 构造空 Prompt 线程允许看到的最小参考上下文。
pub(super) fn reference_items(store: &ThreadEventStore) -> Vec<ResponseItem> {
    let mut persisted_latest = None;
    for turn in &store.turns {
        let candidate = RecentPromptTurn::from_turn(turn);
        if candidate.has_user_message() {
            persisted_latest = Some(candidate);
        }
    }

    // 实时 turn 可能尚未合并进内存 turns；事件缓冲只投影 typed 用户和助手条目，主动排除导致
    // 原项目规则上下文串线的内部活动。
    let mut buffered_latest = None;
    for event in &store.buffer {
        match event {
            ThreadBufferedEvent::Notification(notification) => match notification.as_ref() {
                ServerNotification::TurnStarted(notification) => {
                    buffered_latest = Some(RecentPromptTurn::from_turn(&notification.turn));
                }
                ServerNotification::ItemCompleted(notification)
                    if is_prompt_reference_item(&notification.item) =>
                {
                    let turn = buffered_latest.get_or_insert_with(|| RecentPromptTurn {
                        id: notification.turn_id.clone(),
                        ..RecentPromptTurn::default()
                    });
                    if turn.id != notification.turn_id {
                        *turn = RecentPromptTurn {
                            id: notification.turn_id.clone(),
                            ..RecentPromptTurn::default()
                        };
                    }
                    turn.absorb_item(&notification.item);
                }
                _ => {}
            },
            ThreadBufferedEvent::Request(_)
            | ThreadBufferedEvent::HistoryEntryResponse(_)
            | ThreadBufferedEvent::FeedbackSubmission(_) => {}
        }
    }

    let candidate = match buffered_latest {
        Some(buffered) if buffered.has_user_message() => {
            // 缓冲区中的同一 turn 代表更近的实时状态。不能把持久化快照里的旧助手结果
            // 拼回来，否则当前用户消息尚未完成时会产生错误的请求/结果配对。
            Some(buffered)
        }
        _ => persisted_latest,
    };
    let Some(candidate) = candidate else {
        return Vec::new();
    };

    let Some(user_text) = candidate.user_text else {
        return Vec::new();
    };
    let user_text = truncate_text(&user_text);
    let mut items = vec![text_message(
        "user",
        format!("{REFERENCE_CONTEXT_HEADER}\n\nMost recent user request:\n{user_text}"),
    )];
    if let Some(assistant_text) = candidate.assistant_text {
        items.push(text_message(
            "assistant",
            format!(
                "Most recent assistant result (reference only):\n{}",
                truncate_text(&assistant_text)
            ),
        ));
    }
    items
}

fn is_prompt_reference_item(item: &ThreadItem) -> bool {
    matches!(
        item,
        ThreadItem::UserMessage { .. } | ThreadItem::AgentMessage { .. }
    )
}

fn user_message_text(content: &[codex_app_server_protocol::UserInput]) -> Option<String> {
    let mut text = String::new();
    for input in content {
        if let codex_app_server_protocol::UserInput::Text {
            text: input_text, ..
        } = input
        {
            text.push_str(input_text);
        }
    }
    (!text.trim().is_empty()).then_some(text)
}

fn text_message(role: &str, text: String) -> ResponseItem {
    let content = match role {
        // Responses API 按消息角色区分输入与输出内容；助手参考结果必须使用
        // output_text，否则上游会把 assistant + input_text 判定为非法请求。
        "assistant" => ContentItem::OutputText { text },
        _ => ContentItem::InputText { text },
    };
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![content],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn truncate_text(text: &str) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(MAX_REFERENCE_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...[truncated]")
    } else {
        truncated
    }
}

#[cfg(test)]
#[path = "prompt_context_tests.rs"]
mod tests;
