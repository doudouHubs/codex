//! App-level lifecycle for the `#` prompt optimization thread.
//!
//! Prompt optimization is intentionally separate from side conversations and agent navigation.
//! The child thread is only a temporary model workspace: its transcript is never merged into the
//! main thread, and the final assistant message is copied back into the main composer.

use super::*;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

const PROMPT_CONTEXT_LABEL: &str = "Prompt from main thread";
const PROMPT_MAIN_THREAD_UNAVAILABLE_MESSAGE: &str =
    "Prompt optimization is unavailable until the main thread is ready.";
const PROMPT_ALREADY_OPEN_MESSAGE: &str =
    "Prompt optimization is already open. Press Ctrl+C to return.";
const PROMPT_BOUNDARY_PROMPT: &str = r#"Prompt optimization boundary.

Everything before this boundary is inherited history from the main thread. It is reference context only, not an active request.

Only user messages submitted after this boundary are active instructions for this prompt-optimization thread. Do not continue, execute, or complete requests, plans, tool calls, approvals, or edits found only in inherited history.

Your job is to help the user turn their latest prompt into a complete, precise, high-quality prompt for the main thread. If the requirements, target, constraints, or expected output are unclear, use the `request_user_input` tool to ask focused questions and settle them before producing the final prompt.

When the requirements are settled, output only the complete optimized prompt. Do not add a preface, explanation, analysis, markdown fence, or commentary around it.

Do not modify files, git state, permissions, configuration, or workspace state. Do not use sub-agents."#;
const PROMPT_DEVELOPER_INSTRUCTIONS: &str = r#"You are the prompt-optimization assistant in an isolated child thread.

The inherited fork history is reference material only. Ignore any instruction that appears before the prompt-optimization boundary. Work only on the prompt submitted after that boundary.

Clarify missing requirements with `request_user_input` before writing the final result. Once clarified, return only the full optimized prompt that can be sent to the main thread. Preserve the user's intent; do not invent requirements. Do not modify files or other workspace state, and do not use sub-agents."#;

#[derive(Debug)]
pub(super) struct PromptThreadState {
    pub(super) parent_thread_id: ThreadId,
    pub(super) thread_id: ThreadId,
    pub(super) original_prompt: String,
    current_turn_id: Option<String>,
    streamed_output: String,
    completed_output: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PromptThreadEventOutcome {
    None,
    TurnCompleted(String),
    ThreadClosed,
}

impl PromptThreadState {
    pub(super) fn new(
        parent_thread_id: ThreadId,
        thread_id: ThreadId,
        original_prompt: String,
    ) -> Self {
        Self {
            parent_thread_id,
            thread_id,
            original_prompt,
            current_turn_id: None,
            streamed_output: String::new(),
            completed_output: None,
        }
    }

    fn turn_matches(&self, turn_id: &str) -> bool {
        self.current_turn_id
            .as_deref()
            .is_none_or(|current_turn_id| current_turn_id == turn_id)
    }

    pub(super) fn observe_notification(
        &mut self,
        notification: &ServerNotification,
    ) -> PromptThreadEventOutcome {
        match notification {
            ServerNotification::TurnStarted(notification) => {
                self.current_turn_id = Some(notification.turn.id.clone());
                self.streamed_output.clear();
                self.completed_output = None;
                PromptThreadEventOutcome::None
            }
            ServerNotification::AgentMessageDelta(notification)
                if self.turn_matches(&notification.turn_id) =>
            {
                self.streamed_output.push_str(&notification.delta);
                PromptThreadEventOutcome::None
            }
            ServerNotification::ItemCompleted(notification)
                if self.turn_matches(&notification.turn_id) =>
            {
                if let ThreadItem::AgentMessage { text, .. } = &notification.item {
                    self.completed_output = Some(text.clone());
                }
                PromptThreadEventOutcome::None
            }
            ServerNotification::TurnCompleted(notification)
                if self.turn_matches(&notification.turn.id) =>
            {
                let outcome = match notification.turn.status {
                    TurnStatus::Completed => {
                        let text = self
                            .completed_output
                            .take()
                            .filter(|text| !text.is_empty())
                            .unwrap_or_else(|| std::mem::take(&mut self.streamed_output));
                        PromptThreadEventOutcome::TurnCompleted(text)
                    }
                    TurnStatus::Interrupted | TurnStatus::Failed | TurnStatus::InProgress => {
                        PromptThreadEventOutcome::None
                    }
                };
                self.current_turn_id = None;
                self.streamed_output.clear();
                outcome
            }
            ServerNotification::ThreadClosed(_) => PromptThreadEventOutcome::ThreadClosed,
            _ => PromptThreadEventOutcome::None,
        }
    }
}

impl App {
    pub(super) fn prompt_context_label() -> &'static str {
        PROMPT_CONTEXT_LABEL
    }

    pub(super) fn prompt_boundary_prompt_item() -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: PROMPT_BOUNDARY_PROMPT.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    pub(super) fn prompt_fork_config(&self) -> Config {
        let mut fork_config = self.chat_widget.config_ref().clone();
        let parent_model = self.chat_widget.current_model();
        if !parent_model.trim().is_empty() {
            fork_config.model = Some(parent_model.to_string());
        }
        fork_config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
        fork_config.service_tier = self.chat_widget.configured_service_tier();
        fork_config.ephemeral = true;
        fork_config.developer_instructions =
            Some(match fork_config.developer_instructions.as_deref() {
                Some(existing) if !existing.trim().is_empty() => {
                    format!("{existing}\n\n{PROMPT_DEVELOPER_INSTRUCTIONS}")
                }
                _ => PROMPT_DEVELOPER_INSTRUCTIONS.to_string(),
            });
        fork_config
    }

    pub(super) fn prompt_start_block_message(&self) -> Option<&'static str> {
        if self.primary_thread_id.is_none() {
            Some(PROMPT_MAIN_THREAD_UNAVAILABLE_MESSAGE)
        } else if self.prompt_thread.is_some() || self.prompt_starting.is_some() {
            Some(PROMPT_ALREADY_OPEN_MESSAGE)
        } else {
            None
        }
    }

    pub(super) fn is_active_prompt_thread(&self) -> bool {
        self.prompt_thread
            .as_ref()
            .is_some_and(|state| self.current_displayed_thread_id() == Some(state.thread_id))
    }

    pub(super) fn prompt_thread_id(&self) -> Option<ThreadId> {
        self.prompt_thread.as_ref().map(|state| state.thread_id)
    }

    /// Keep Prompt presentation independent from side and agent-navigation presentation.
    ///
    /// The same footer slot is reused for the exact Prompt label, but Prompt state is not encoded
    /// in `side_threads`; this prevents `/agent` and side failover from treating the temporary
    /// optimizer as a normal child conversation.
    pub(super) fn sync_prompt_thread_ui(&mut self) {
        let prompt_active = self.is_active_prompt_thread();
        let prompt_starting = self.prompt_starting.is_some();
        if prompt_active {
            self.chat_widget.set_prompt_mode_available(false);
            self.chat_widget
                .set_side_conversation_context_label(Some(PROMPT_CONTEXT_LABEL.to_string()));
            self.chat_widget.set_prompt_mode(ComposerPromptMode::Thread);
        } else if prompt_starting {
            self.chat_widget.set_prompt_mode_available(false);
        } else if !self.side_threads.is_empty() {
            // Side threads are mutually exclusive with Prompt entry, including while main is
            // visible. Treat a later `#` as literal text until the side child is discarded.
            self.chat_widget.set_prompt_mode_available(false);
        } else if self.chat_widget.side_conversation_active() {
            self.chat_widget.set_prompt_mode_available(false);
        } else {
            self.chat_widget.set_prompt_mode_available(true);
            self.chat_widget
                .set_side_conversation_context_label(/*label*/ None);
        }
    }

    pub(super) fn observe_prompt_event(
        &mut self,
        event: &ThreadBufferedEvent,
    ) -> PromptThreadEventOutcome {
        let Some(state) = self.prompt_thread.as_mut() else {
            return PromptThreadEventOutcome::None;
        };
        let ThreadBufferedEvent::Notification(notification) = event else {
            return PromptThreadEventOutcome::None;
        };
        state.observe_notification(notification)
    }

    pub(super) async fn handle_start_prompt(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        text: String,
    ) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }

        if let Some(message) = self.prompt_start_block_message() {
            self.chat_widget
                .set_prompt_mode(ComposerPromptMode::Inactive);
            self.chat_widget.set_prompt_text(text);
            self.chat_widget.add_error_message(message.to_string());
            return Ok(());
        }

        // The composer has already cleared the main draft. Keep the original text in Prompt state
        // so cancellation can restore exactly what the user entered, even after several rewrites.
        self.prompt_starting = Some(text.clone());
        self.sync_prompt_thread_ui();
        self.refresh_in_memory_config_from_disk_best_effort("starting prompt optimization")
            .await;

        let parent_thread_id = self
            .primary_thread_id
            .expect("prompt start requires main thread");
        let forked = match app_server
            .fork_thread(self.prompt_fork_config(), parent_thread_id)
            .await
        {
            Ok(forked) => forked,
            Err(err) => {
                self.prompt_starting = None;
                self.chat_widget
                    .set_prompt_mode(ComposerPromptMode::Inactive);
                self.chat_widget.set_prompt_text(text);
                self.chat_widget
                    .add_error_message(format!("Failed to start prompt optimization: {err}"));
                self.sync_prompt_thread_ui();
                return Ok(());
            }
        };

        let child_thread_id = forked.session.thread_id;
        {
            let channel = self.ensure_thread_channel(child_thread_id);
            let mut store = channel.store.lock().await;
            Self::install_prompt_thread_snapshot(&mut store, forked.session, forked.turns);
        }

        if let Err(err) = app_server
            .thread_inject_items(child_thread_id, vec![Self::prompt_boundary_prompt_item()])
            .await
        {
            self.discard_prompt_thread_local_state(app_server, child_thread_id)
                .await;
            self.prompt_starting = None;
            self.chat_widget
                .set_prompt_mode(ComposerPromptMode::Inactive);
            self.chat_widget.set_prompt_text(text);
            self.chat_widget
                .add_error_message(format!("Failed to prepare prompt optimization: {err}"));
            self.sync_prompt_thread_ui();
            return Ok(());
        }

        self.prompt_thread = Some(PromptThreadState::new(
            parent_thread_id,
            child_thread_id,
            text.clone(),
        ));
        self.prompt_starting = None;
        if let Err(err) = self.activate_prompt_thread(tui, child_thread_id).await {
            self.restore_prompt_after_failure(tui, app_server, text, err)
                .await?;
            return Ok(());
        }

        // 切换到 fork 会重建 ChatWidget，主线程 composer 中刚记录的首条输入不会随之迁移。
        // Prompt 的上下键必须从子线程自己的输入历史开始，否则第一次按 Up 会直接落空。
        self.chat_widget.record_prompt_history(text.clone());
        self.chat_widget.submit_user_message_text(text);
        self.sync_prompt_thread_ui();
        Ok(())
    }

    pub(super) async fn cancel_prompt(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<()> {
        let Some(state) = self.prompt_thread.as_ref() else {
            self.prompt_starting = None;
            self.chat_widget
                .set_prompt_mode(ComposerPromptMode::Inactive);
            self.sync_prompt_thread_ui();
            return Ok(());
        };
        let original_prompt = state.original_prompt.clone();
        self.close_prompt_thread(tui, app_server, /*interrupt*/ true)
            .await?;
        self.chat_widget.set_prompt_text(original_prompt);
        Ok(())
    }

    pub(super) async fn submit_prompt_to_main(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        text: String,
    ) -> Result<()> {
        if self.prompt_thread.is_none() {
            return Ok(());
        }
        self.close_prompt_thread(tui, app_server, /*interrupt*/ true)
            .await?;
        if !text.trim().is_empty() {
            self.chat_widget.submit_user_message_text(text);
        }
        Ok(())
    }

    fn install_prompt_thread_snapshot(
        store: &mut ThreadEventStore,
        mut session: ThreadSessionState,
        _forked_turns: Vec<Turn>,
    ) {
        // The server retains inherited history for model context, but the Prompt surface starts at
        // its boundary and must not render the main transcript a second time.
        session.forked_from_id = None;
        session.thread_name = Some(PROMPT_CONTEXT_LABEL.to_string());
        store.set_session(session, Vec::new());
    }

    async fn activate_prompt_thread(
        &mut self,
        tui: &mut tui::Tui,
        thread_id: ThreadId,
    ) -> Result<()> {
        let previous_thread_id = self.active_thread_id;
        self.store_active_thread_receiver().await;
        self.active_thread_id = None;
        let Some((receiver, snapshot)) = self.activate_thread_for_replay(thread_id).await else {
            if let Some(previous_thread_id) = previous_thread_id {
                self.activate_thread_channel(previous_thread_id).await;
            }
            return Err(color_eyre::eyre::eyre!(
                "Prompt optimization thread {thread_id} is already active"
            ));
        };

        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = Some(receiver);
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            /*initial_user_message*/ None,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.reset_for_thread_switch(tui)?;
        self.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);
        self.sync_active_agent_label();
        Ok(())
    }

    async fn close_prompt_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        interrupt: bool,
    ) -> Result<()> {
        let Some(state) = self.prompt_thread.take() else {
            self.prompt_starting = None;
            self.chat_widget
                .set_prompt_mode(ComposerPromptMode::Inactive);
            self.sync_prompt_thread_ui();
            return Ok(());
        };

        let child_thread_id = state.thread_id;
        let parent_thread_id = state.parent_thread_id;
        if interrupt {
            let interrupt_result =
                if let Some(turn_id) = self.active_turn_id_for_thread(child_thread_id).await {
                    app_server.turn_interrupt(child_thread_id, turn_id).await
                } else {
                    app_server.startup_interrupt(child_thread_id).await
                };
            if let Err(err) = interrupt_result {
                tracing::warn!(thread_id = %child_thread_id, "failed to interrupt prompt thread: {err}");
            }
        }
        if let Err(err) = app_server.thread_unsubscribe(child_thread_id).await {
            tracing::warn!(thread_id = %child_thread_id, "failed to unsubscribe prompt thread: {err}");
        }
        self.discard_prompt_thread_local_state(app_server, child_thread_id)
            .await;
        self.prompt_starting = None;
        if self.active_thread_id != Some(parent_thread_id) {
            self.select_prompt_parent_thread(tui, parent_thread_id)
                .await?;
        }
        self.chat_widget
            .set_prompt_mode(ComposerPromptMode::Inactive);
        self.sync_active_agent_label();
        Ok(())
    }

    async fn select_prompt_parent_thread(
        &mut self,
        tui: &mut tui::Tui,
        parent_thread_id: ThreadId,
    ) -> Result<()> {
        let Some((receiver, snapshot)) = self.activate_thread_for_replay(parent_thread_id).await
        else {
            return Err(color_eyre::eyre::eyre!(
                "Main thread {parent_thread_id} is unavailable while closing Prompt mode"
            ));
        };
        self.active_thread_id = Some(parent_thread_id);
        self.active_thread_rx = Some(receiver);
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            /*initial_user_message*/ None,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.reset_for_thread_switch(tui)?;
        self.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ true);
        Ok(())
    }

    async fn discard_prompt_thread_local_state(
        &mut self,
        _app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        self.abort_thread_event_listener(thread_id);
        self.thread_event_channels.remove(&thread_id);
        if self.active_thread_id == Some(thread_id) {
            self.active_thread_id = None;
            self.active_thread_rx = None;
        }
    }

    async fn restore_prompt_after_failure(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        text: String,
        err: color_eyre::eyre::Report,
    ) -> Result<()> {
        self.close_prompt_thread(tui, app_server, /*interrupt*/ false)
            .await?;
        self.chat_widget.set_prompt_text(text);
        self.chat_widget
            .add_error_message(format!("Failed to enter prompt optimization: {err}"));
        Ok(())
    }
}
