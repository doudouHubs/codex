use super::App;
use super::PromptOptimizationMode;
use super::PromptThreadState;
use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

fn mode_instruction(mode: PromptOptimizationMode) -> String {
    let ResponseItem::Message { role, content, .. } = App::prompt_optimization_mode_item(mode)
    else {
        panic!("prompt mode must be represented by a message item");
    };
    assert_eq!(role, "developer");
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("prompt mode message must contain one text item");
    };
    text.clone()
}

#[test]
fn prompt_modes_use_distinct_developer_instructions() {
    let fast = mode_instruction(PromptOptimizationMode::Fast);
    let full = mode_instruction(PromptOptimizationMode::Full);

    assert!(fast.contains("FAST"));
    assert!(full.contains("FULL"));
    assert_ne!(fast, full);
}

#[test]
fn full_mode_requires_contextual_non_template_expansion() {
    let full = mode_instruction(PromptOptimizationMode::Full);

    for required_phrase in [
        "materially improved prompt",
        "never echo the input unchanged",
        "user's language, tone, domain, and natural prompt form",
        "Do not force every prompt into a universal template",
        "Use paragraphs, bullets, or other organization only when they fit",
        "Expand only relevant implied details",
        "do not add arbitrary requirements",
        "safe common default",
        "request_user_input",
    ] {
        assert!(
            full.contains(required_phrase),
            "Full mode instructions should contain {required_phrase:?}"
        );
    }

    assert!(!full.contains("include a clearly labeled `Assumptions` section"));
    assert!(!full.contains("structured, directly usable request"));
}

#[test]
fn prompt_boundary_defines_safe_default_and_tool_policy() {
    let ResponseItem::Message { role, content, .. } = App::prompt_boundary_prompt_item() else {
        panic!("prompt boundary must be represented by a message item");
    };
    assert_eq!(role, "user");
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("prompt boundary must contain one text item");
    };

    assert!(text.contains("safely defaulted"));
    assert!(text.contains("request_user_input"));
    assert!(text.contains("Do not return the input unchanged"));
}

#[test]
fn fast_mode_stays_concise_and_does_not_answer_the_underlying_task() {
    let fast = mode_instruction(PromptOptimizationMode::Fast);

    assert!(fast.contains("close to the original length"));
    assert!(fast.contains("Return a rewrite, not an answer"));
    assert!(!fast.contains("never echo the input unchanged"));
}

#[test]
fn prompt_thread_state_keeps_the_selected_optimization_mode() {
    let state = PromptThreadState::new(
        ThreadId::new(),
        ThreadId::new(),
        "original prompt".to_string(),
        PromptOptimizationMode::Fast,
    );

    assert_eq!(state.optimization_mode, PromptOptimizationMode::Fast);
}
