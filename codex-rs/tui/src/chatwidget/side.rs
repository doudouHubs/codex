//! Chat widget hooks for side-conversation mode.
//!
//! App-level side-thread lifecycle lives in `app::side`; this module owns the
//! chat-surface pieces that side mode toggles, such as the composer placeholder,
//! footer label, and inline `/side` message submission behavior.

use super::*;
use crate::bottom_pane::ComposerPromptMode;

impl ChatWidget {
    pub(crate) fn submit_user_message_as_plain_user_turn(
        &mut self,
        user_message: UserMessage,
    ) -> Option<AppCommand> {
        self.submit_user_message_with_shell_escape_policy(user_message, ShellEscapePolicy::Disallow)
    }

    pub(crate) fn set_side_conversation_active(&mut self, active: bool) {
        self.active_side_conversation = active;
        let placeholder = if active {
            self.side_placeholder_text.clone()
        } else {
            self.normal_placeholder_text.clone()
        };
        self.bottom_pane.set_placeholder_text(placeholder);
        self.bottom_pane.set_side_conversation_active(active);
        // Prompt and side are mutually exclusive surfaces. The App layer re-enables Prompt after
        // returning to main, while side activation closes the entry mode immediately.
        self.bottom_pane.set_prompt_mode_available(!active);
    }

    pub(crate) fn side_conversation_active(&self) -> bool {
        self.active_side_conversation
    }

    pub(crate) fn prompt_mode(&self) -> ComposerPromptMode {
        self.bottom_pane.prompt_mode()
    }

    pub(crate) fn set_prompt_mode(&mut self, mode: ComposerPromptMode) {
        self.bottom_pane.set_prompt_mode(mode);
    }

    pub(crate) fn set_prompt_mode_available(&mut self, available: bool) {
        self.bottom_pane.set_prompt_mode_available(available);
    }

    /// 供 App 层快捷键复用 `/btw` 的 review 限制，避免绕过 slash command 的可用性检查。
    pub(crate) fn can_start_side_conversation(&self) -> bool {
        !self.active_side_conversation && !self.review.is_review_mode
    }

    pub(crate) fn set_side_conversation_context_label(&mut self, label: Option<String>) {
        self.bottom_pane.set_side_conversation_context_label(label);
    }
}
