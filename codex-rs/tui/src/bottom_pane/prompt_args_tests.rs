use super::PromptInput;
use super::PromptOptimizationMode;
use super::parse_prompt_input;
use pretty_assertions::assert_eq;

#[test]
fn prompt_input_defaults_to_full_mode() {
    assert_eq!(
        parse_prompt_input("make this request precise", PromptOptimizationMode::Full),
        PromptInput {
            text: "make this request precise".to_string(),
            optimization_mode: PromptOptimizationMode::Full,
        }
    );
}

#[test]
fn prompt_input_consumes_fast_and_full_suffixes() {
    assert_eq!(
        parse_prompt_input("make it precise --fast", PromptOptimizationMode::Full),
        PromptInput {
            text: "make it precise".to_string(),
            optimization_mode: PromptOptimizationMode::Fast,
        }
    );
    assert_eq!(
        parse_prompt_input("make it detailed --full", PromptOptimizationMode::Fast),
        PromptInput {
            text: "make it detailed".to_string(),
            optimization_mode: PromptOptimizationMode::Full,
        }
    );
}

#[test]
fn prompt_input_uses_the_last_valid_suffix_parameter() {
    assert_eq!(
        parse_prompt_input(
            "keep this concise --fast --full",
            PromptOptimizationMode::Fast
        ),
        PromptInput {
            text: "keep this concise".to_string(),
            optimization_mode: PromptOptimizationMode::Full,
        }
    );
}

#[test]
fn prompt_input_preserves_unknown_or_non_suffix_tokens() {
    assert_eq!(
        parse_prompt_input("keep --unknown --fast", PromptOptimizationMode::Full),
        PromptInput {
            text: "keep --unknown".to_string(),
            optimization_mode: PromptOptimizationMode::Fast,
        }
    );
    assert_eq!(
        parse_prompt_input("keep --fast --unknown", PromptOptimizationMode::Full),
        PromptInput {
            text: "keep --fast --unknown".to_string(),
            optimization_mode: PromptOptimizationMode::Full,
        }
    );
    assert_eq!(
        parse_prompt_input("keep --fast,", PromptOptimizationMode::Full),
        PromptInput {
            text: "keep --fast,".to_string(),
            optimization_mode: PromptOptimizationMode::Full,
        }
    );
}

#[test]
fn prompt_input_keeps_the_current_mode_without_a_parameter() {
    assert_eq!(
        parse_prompt_input("continue this draft", PromptOptimizationMode::Fast),
        PromptInput {
            text: "continue this draft".to_string(),
            optimization_mode: PromptOptimizationMode::Fast,
        }
    );
}
