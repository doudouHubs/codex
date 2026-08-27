use super::*;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::DynamicToolCallItem;
use codex_protocol::items::DynamicToolCallStatus;
use codex_protocol::items::PlanItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::protocol::AgentMessageEvent;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::PlanDeltaEvent;
use codex_protocol::protocol::UserMessageEvent;
use codex_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;
use std::cell::RefCell;

#[derive(Default)]
struct RecordingSink {
    thread_id: RefCell<Option<String>>,
    activities: RefCell<Vec<(ActivityStatus, ProcessMode, Option<String>)>>,
    errors: RefCell<Vec<String>>,
    prompt: RefCell<Option<String>>,
    plan_text: RefCell<Option<String>>,
    plans: RefCell<Vec<Vec<PlanStep>>>,
    messages: RefCell<Vec<(String, String)>>,
    tool_calls: RefCell<Vec<ToolCallRecord>>,
}

impl SupervisorEventSink for RecordingSink {
    fn set_thread_id(&self, thread_id: Option<String>) {
        *self.thread_id.borrow_mut() = thread_id;
    }

    fn set_activity(&self, activity: ActivityStatus, mode: ProcessMode, summary: Option<String>) {
        self.activities.borrow_mut().push((activity, mode, summary));
    }

    fn set_error(&self, error: String) {
        self.errors.borrow_mut().push(error);
    }

    fn set_prompt(&self, prompt: String) {
        *self.prompt.borrow_mut() = Some(prompt);
    }

    fn set_plan_text(&self, plan_text: String) {
        *self.plan_text.borrow_mut() = Some(plan_text);
    }

    fn append_plan_delta(&self, delta: String) {
        self.plan_text
            .borrow_mut()
            .get_or_insert_with(String::new)
            .push_str(&delta);
    }

    fn set_plan(&self, plan: Vec<PlanStep>) {
        self.plans.borrow_mut().push(plan);
    }

    fn append_message(&self, role: String, content: String) {
        self.messages.borrow_mut().push((role, content));
    }

    fn upsert_tool_call(
        &self,
        id: String,
        name: String,
        input: String,
        output: Option<String>,
        status: String,
    ) {
        self.tool_calls.borrow_mut().push(ToolCallRecord {
            id: Some(id),
            name,
            input,
            output,
            status,
            created_at: None,
        });
    }
}

fn item_started(thread_id: ThreadId, turn_id: &str, item: TurnItem) -> EventMsg {
    EventMsg::ItemStarted(ItemStartedEvent {
        thread_id,
        turn_id: turn_id.to_string(),
        item,
        started_at_ms: 1,
    })
}

fn item_completed(thread_id: ThreadId, turn_id: &str, item: TurnItem) -> EventMsg {
    EventMsg::ItemCompleted(ItemCompletedEvent {
        thread_id,
        turn_id: turn_id.to_string(),
        item,
        started_at_ms: Some(1),
        completed_at_ms: 2,
    })
}

#[test]
fn canonical_user_and_agent_items_are_recorded_once() {
    let sink = RecordingSink::default();
    let thread_id = ThreadId::from_u128(1);
    let user = UserMessageItem::new(&[UserInput::Text {
        text: "inspect the supervisor".to_string(),
        text_elements: Vec::new(),
    }]);
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &item_started(thread_id, "turn-1", TurnItem::UserMessage(user.clone())),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &item_completed(thread_id, "turn-1", TurnItem::UserMessage(user)),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &EventMsg::UserMessage(UserMessageEvent {
            message: "inspect the supervisor".to_string(),
            ..UserMessageEvent::default()
        }),
    );

    let agent = AgentMessageItem {
        id: "message-1".to_string(),
        content: vec![AgentMessageContent::Text {
            text: "I found the supervisor.".to_string(),
        }],
        phase: None,
        memory_citation: None,
    };
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &item_started(thread_id, "turn-1", TurnItem::AgentMessage(agent.clone())),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &item_completed(thread_id, "turn-1", TurnItem::AgentMessage(agent)),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &EventMsg::AgentMessage(AgentMessageEvent {
            message: "I found the supervisor.".to_string(),
            phase: None,
            memory_citation: None,
        }),
    );

    assert_eq!(*sink.thread_id.borrow(), Some(thread_id.to_string()));
    assert_eq!(
        *sink.prompt.borrow(),
        Some("inspect the supervisor".to_string())
    );
    assert_eq!(
        *sink.messages.borrow(),
        vec![
            ("user".to_string(), "inspect the supervisor".to_string()),
            (
                "assistant".to_string(),
                "I found the supervisor.".to_string()
            ),
        ]
    );
}

#[test]
fn canonical_plan_text_and_update_plan_are_kept_separately() {
    let sink = RecordingSink::default();
    let thread_id = ThreadId::from_u128(2);
    let plan = PlanItem {
        id: "plan-1".to_string(),
        text: String::new(),
    };
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Plan,
        &item_started(thread_id, "turn-2", TurnItem::Plan(plan.clone())),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Plan,
        &EventMsg::PlanDelta(PlanDeltaEvent {
            thread_id: thread_id.to_string(),
            turn_id: "turn-2".to_string(),
            item_id: "plan-1".to_string(),
            delta: "draft\n".to_string(),
        }),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Plan,
        &item_completed(
            thread_id,
            "turn-2",
            TurnItem::Plan(PlanItem {
                text: "final proposal".to_string(),
                ..plan
            }),
        ),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Plan,
        &EventMsg::PlanUpdate(UpdatePlanArgs {
            explanation: Some("execution checklist".to_string()),
            plan: vec![PlanItemArg {
                step: "read the worker state".to_string(),
                status: codex_protocol::plan_tool::StepStatus::InProgress,
            }],
        }),
    );

    assert_eq!(*sink.plan_text.borrow(), Some("final proposal".to_string()));
    assert_eq!(
        *sink.plans.borrow(),
        vec![vec![PlanStep {
            step: "read the worker state".to_string(),
            status: "inProgress".to_string(),
        }]]
    );
}

#[test]
fn canonical_tool_lifecycle_keeps_the_call_id() {
    let sink = RecordingSink::default();
    let thread_id = ThreadId::from_u128(3);
    let started = DynamicToolCallItem {
        id: "call-1".to_string(),
        namespace: Some("workspace".to_string()),
        tool: "inspect".to_string(),
        arguments: serde_json::json!({"path": "."}),
        status: DynamicToolCallStatus::InProgress,
        content_items: None,
        success: None,
        error: None,
        duration: None,
    };
    let completed = DynamicToolCallItem {
        status: DynamicToolCallStatus::Completed,
        content_items: Some(Vec::new()),
        success: Some(true),
        ..started.clone()
    };

    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &item_started(thread_id, "turn-3", TurnItem::DynamicToolCall(started)),
    );
    record_event_with_sink(
        &sink,
        thread_id,
        ModeKind::Default,
        &item_completed(thread_id, "turn-3", TurnItem::DynamicToolCall(completed)),
    );

    let calls = sink.tool_calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, Some("call-1".to_string()));
    assert_eq!(calls[0].status, "running");
    assert_eq!(calls[1].id, Some("call-1".to_string()));
    assert_eq!(calls[1].status, "completed");
}
