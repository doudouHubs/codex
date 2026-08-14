/// Parse a first-line slash command of the form `/name <rest>`.
/// Returns `(name, rest_after_name, rest_offset)` if the line begins with `/`
/// and contains a non-empty name; otherwise returns `None`.
///
/// `rest_offset` is the byte index into the original line where `rest_after_name`
/// starts after trimming leading whitespace (so `line[rest_offset..] == rest_after_name`).
pub fn parse_slash_name(line: &str) -> Option<(&str, &str, usize)> {
    let stripped = line.strip_prefix('/')?;
    let mut name_end_in_stripped = stripped.len();
    for (idx, ch) in stripped.char_indices() {
        if ch.is_whitespace() {
            name_end_in_stripped = idx;
            break;
        }
    }
    let name = &stripped[..name_end_in_stripped];
    if name.is_empty() {
        return None;
    }
    let rest_untrimmed = &stripped[name_end_in_stripped..];
    let rest = rest_untrimmed.trim_start();
    let rest_start_in_stripped = name_end_in_stripped + (rest_untrimmed.len() - rest.len());
    let rest_offset = rest_start_in_stripped + 1;
    Some((name, rest, rest_offset))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PromptOptimizationMode {
    Fast,
    #[default]
    Full,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PromptInput {
    pub(crate) text: String,
    pub(crate) optimization_mode: PromptOptimizationMode,
}

/// 解析 `#` 模式末尾的优化参数，并把控制参数从实际提示词正文中移除。
///
/// 只消费末尾连续的合法 token，避免把正文中用于描述命令行参数的 `--fast` 误删；
/// 从右向左扫描时第一次遇到的合法参数就是用户输入中最后一个参数，因此重复参数遵循
/// “最后一个生效”的规则。未知 token 会停留在正文中。
pub(crate) fn parse_prompt_input(input: &str, current_mode: PromptOptimizationMode) -> PromptInput {
    let mut body_end = input.trim_end().len();
    let mut optimization_mode = current_mode;
    let mut mode_was_selected = false;

    while body_end > 0 {
        let token_start = input[..body_end]
            .char_indices()
            .rev()
            .find_map(|(index, character)| {
                character
                    .is_whitespace()
                    .then_some(index + character.len_utf8())
            })
            .unwrap_or(0);
        let token = &input[token_start..body_end];
        let Some(token_mode) = (match token {
            "--fast" => Some(PromptOptimizationMode::Fast),
            "--full" => Some(PromptOptimizationMode::Full),
            _ => None,
        }) else {
            break;
        };

        if !mode_was_selected {
            optimization_mode = token_mode;
            mode_was_selected = true;
        }
        body_end = input[..token_start].trim_end().len();
    }

    PromptInput {
        text: input[..body_end].trim().to_string(),
        optimization_mode,
    }
}

#[cfg(test)]
#[path = "prompt_args_tests.rs"]
mod tests;
