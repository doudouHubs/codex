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
        "user's language, tone, and domain",
        "creative requests",
        "clarify the intended outcome",
        "directly implied context, audience, inputs, scope, or domain terms",
        "relevant quality bar",
        "each bullet focused on one idea",
        "organizing the result for quick reading",
        "each point MUST be on its own line beginning with `- `",
        "do not merge those points into one prose sentence",
        "Use a numbered list only when the points have a meaningful execution order",
        "use concise headings or short paragraphs",
        "short prompts do not need artificial sections",
        "Do not force every prompt into a universal template",
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
fn prompt_boundary_prioritizes_current_input_over_sanitized_reference() {
    let ResponseItem::Message { role, content, .. } = App::prompt_boundary_prompt_item() else {
        panic!("prompt boundary must be represented by a message item");
    };
    assert_eq!(role, "user");
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("prompt boundary must contain one text item");
    };

    for required_phrase in [
        "bounded, sanitized excerpt",
        "authoritative current prompt-optimization request",
        "If the current input is a complete prompt",
        "do not replace it with the reference context",
        "If the current input is a short direction or modification",
        "most recent relevant user request and assistant result",
        "AGENTS.md",
        "system or developer rules",
        "request_user_input",
        "safely defaulted",
    ] {
        assert!(
            text.contains(required_phrase),
            "Prompt boundary should contain {required_phrase:?}"
        );
    }

    assert!(!text.contains("Everything before this boundary is inherited history"));
    assert!(!text.contains("Use relevant inherited history as source material"));
}

#[tokio::test]
async fn prompt_thread_config_drops_parent_developer_instructions() {
    let app = crate::app::test_support::make_test_app().await;

    let config = app.prompt_thread_config();
    let developer_instructions = config
        .developer_instructions
        .expect("Prompt thread should have its own developer instructions");

    assert!(developer_instructions.contains("prompt-optimization assistant"));
    assert!(!developer_instructions.contains("main project policy"));
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
        Vec::new(),
        Vec::new(),
        PromptOptimizationMode::Fast,
    );

    assert_eq!(state.optimization_mode, PromptOptimizationMode::Fast);
}
