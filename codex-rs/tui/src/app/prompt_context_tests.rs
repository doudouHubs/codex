use super::*;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use pretty_assertions::assert_eq;

fn turn(id: &str, items: Vec<ThreadItem>) -> Turn {
    Turn {
        id: id.to_string(),
        items,
        items_view: TurnItemsView::Full,
        status: TurnStatus::Completed,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

fn user_message(id: &str, text: &str) -> ThreadItem {
    ThreadItem::UserMessage {
        id: id.to_string(),
        client_id: None,
        content: vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
    }
}

fn agent_message(id: &str, text: &str) -> ThreadItem {
    ThreadItem::AgentMessage {
        id: id.to_string(),
        text: text.to_string(),
        phase: None,
        memory_citation: None,
    }
}

fn response_text(item: &ResponseItem) -> &str {
    let ResponseItem::Message { content, .. } = item else {
        panic!("reference context should contain only message items");
    };
    let [ContentItem::InputText { text } | ContentItem::OutputText { text }] = content.as_slice()
    else {
        panic!("reference message should contain one text item");
    };
    text
}

#[test]
fn reference_assistant_result_uses_output_text_content() {
    let mut store = ThreadEventStore::new(/*capacity*/ 8);
    store.set_turns(vec![turn(
        "latest",
        vec![
            user_message("latest-user", "latest request"),
            agent_message("latest-agent", "latest result"),
        ],
    )]);

    let items = reference_items(&store);

    assert_eq!(
        items,
        vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: format!(
                        "{REFERENCE_CONTEXT_HEADER}\n\nMost recent user request:\nlatest request"
                    ),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Most recent assistant result (reference only):\nlatest result"
                        .to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
        ]
    );
}

#[test]
fn reference_items_keep_only_the_latest_user_request_and_agent_result() {
    let mut store = ThreadEventStore::new(/*capacity*/ 8);
    store.set_turns(vec![
        turn(
            "old",
            vec![
                user_message("old-user", "old request"),
                agent_message("old-agent", "old result"),
            ],
        ),
        turn(
            "latest",
            vec![
                user_message("latest-user", "latest request"),
                ThreadItem::Reasoning {
                    id: "reasoning".to_string(),
                    summary: vec!["internal reasoning".to_string()],
                    content: vec!["do not leak this".to_string()],
                },
                ThreadItem::Plan {
                    id: "plan".to_string(),
                    text: "do not treat this as the prompt".to_string(),
                },
                agent_message("latest-agent", "latest result"),
            ],
        ),
    ]);

    let items = reference_items(&store);

    assert_eq!(items.len(), 2);
    assert_eq!(response_text(&items[0]).contains("latest request"), true);
    assert_eq!(response_text(&items[0]).contains("old request"), false);
    assert_eq!(
        response_text(&items[0]).contains("internal reasoning"),
        false
    );
    assert_eq!(
        response_text(&items[0]).contains("do not treat this as the prompt"),
        false
    );
    assert_eq!(
        response_text(&items[1]),
        "Most recent assistant result (reference only):\nlatest result"
    );
}

#[test]
fn reference_items_include_a_live_turn_before_it_enters_turn_storage() {
    let mut store = ThreadEventStore::new(/*capacity*/ 8);
    store.set_turns(vec![turn(
        "old",
        vec![
            user_message("old-user", "old request"),
            agent_message("old-agent", "old result"),
        ],
    )]);
    store.push_notification(ServerNotification::TurnStarted(TurnStartedNotification {
        thread_id: "main".to_string(),
        turn: turn("live", Vec::new()),
    }));
    store.push_notification(ServerNotification::ItemCompleted(
        ItemCompletedNotification {
            item: user_message("live-user", "live request"),
            thread_id: "main".to_string(),
            turn_id: "live".to_string(),
            completed_at_ms: 0,
        },
    ));
    store.push_notification(ServerNotification::ItemCompleted(
        ItemCompletedNotification {
            item: agent_message("live-agent", "live result"),
            thread_id: "main".to_string(),
            turn_id: "live".to_string(),
            completed_at_ms: 1,
        },
    ));

    let items = reference_items(&store);

    assert_eq!(items.len(), 2);
    assert!(response_text(&items[0]).contains("live request"));
    assert!(response_text(&items[1]).contains("live result"));
    assert!(!response_text(&items[0]).contains("old request"));
}

#[test]
fn reference_items_bound_long_text_without_splitting_utf8() {
    let long_request = "提示词".repeat(MAX_REFERENCE_MESSAGE_CHARS);
    let mut store = ThreadEventStore::new(/*capacity*/ 8);
    store.set_turns(vec![turn(
        "latest",
        vec![user_message("latest-user", &long_request)],
    )]);

    let items = reference_items(&store);

    assert_eq!(items.len(), 1);
    let text = response_text(&items[0]);
    assert!(text.ends_with("...[truncated]"));
    let prefix = format!("{REFERENCE_CONTEXT_HEADER}\n\nMost recent user request:\n");
    let payload = text
        .strip_prefix(&prefix)
        .expect("reference message should have the expected header");
    assert_eq!(
        payload.chars().count(),
        MAX_REFERENCE_MESSAGE_CHARS + "...[truncated]".chars().count()
    );
}

#[test]
fn reference_items_are_empty_without_a_user_message() {
    let mut store = ThreadEventStore::new(/*capacity*/ 8);
    store.set_turns(vec![turn(
        "internal-only",
        vec![
            ThreadItem::Reasoning {
                id: "reasoning".to_string(),
                summary: Vec::new(),
                content: vec!["internal".to_string()],
            },
            ThreadItem::Plan {
                id: "plan".to_string(),
                text: "internal plan".to_string(),
            },
        ],
    )]);

    assert!(reference_items(&store).is_empty());
}
