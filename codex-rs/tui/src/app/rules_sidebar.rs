use std::time::Duration;

use super::*;
use crate::pager_overlay::ScrollDestination;
use crate::rules_sidebar::RulesSidebarLoad;
use crate::rules_sidebar::RulesSidebarState;
use crate::rules_sidebar::load_rules;
use crate::rules_sidebar::locate_rule_system_binary;

const RULES_POLL_INTERVAL: Duration = Duration::from_secs(2);

impl App {
    pub(super) fn alt_screen_surface_active(&self) -> bool {
        self.overlay.is_some() || self.rules_sidebar.is_some() || self.main_transcript.is_some()
    }

    pub(super) fn open_rules_sidebar(&mut self, tui: &mut tui::Tui) {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_info_message(
                "Rules are available after the session has started.".to_string(),
                /*hint*/ None,
            );
            return;
        };
        if self.main_transcript.is_some() {
            self.close_main_transcript_viewport(tui);
        }
        // sidebar 不拥有键盘焦点，禁用 wheel->arrow 转换，避免滚轮误触 composer 输入历史。
        let _ = tui.enter_alt_screen_without_alternate_scroll();
        // 侧栏关闭 alternate-scroll 后，普通滚轮必须以 MouseEvent 进入应用，不能等 Ctrl 捕获。
        let _ = tui.enable_mouse_capture_always();
        self.rules_sidebar_generation = self.rules_sidebar_generation.wrapping_add(1);
        self.rules_sidebar = Some(RulesSidebarState::new(
            thread_id,
            self.config.cwd.to_path_buf(),
            self.transcript_cells.clone(),
            self.keymap.pager.clone(),
        ));
        self.spawn_rules_sidebar_load(Duration::ZERO);
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn close_rules_sidebar(&mut self, tui: &mut tui::Tui) {
        if self.rules_sidebar.take().is_none() {
            return;
        }
        self.rules_sidebar_generation = self.rules_sidebar_generation.wrapping_add(1);
        let _ = tui.leave_alt_screen();
        // 侧栏关闭后回到主聊天区，恢复主 composer 所需的鼠标捕获。
        let _ = tui.enable_mouse_capture();
        if !self.deferred_history_lines.is_empty() {
            let lines = std::mem::take(&mut self.deferred_history_lines);
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                lines,
                self.history_line_wrap_policy(),
            );
        }
        tui.frame_requester().schedule_frame();
    }

    fn spawn_rules_sidebar_load(&mut self, delay: Duration) {
        let skill_path = self.chat_widget.rule_system_list_skill_path();
        let executable = locate_rule_system_binary(skill_path);
        let Some(state) = self.rules_sidebar.as_mut() else {
            return;
        };
        if delay.is_zero() {
            // 轮询等待期间继续展示上次成功结果；否则空规则会在每轮完成后立刻退回 Loading。
            state.begin_load();
        }
        let thread_id = state.thread_id();
        let project_root = state.project_root().to_path_buf();
        let generation = self.rules_sidebar_generation;
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let result = match executable {
                Ok(executable) => load_rules(executable, thread_id, project_root).await,
                Err(error) => Err(error),
            };
            app_event_tx.send(AppEvent::RulesSidebarLoaded { generation, result });
        });
    }

    pub(super) fn handle_rules_sidebar_loaded(
        &mut self,
        tui: &mut tui::Tui,
        generation: u64,
        result: Result<RulesSidebarLoad, String>,
    ) {
        if generation != self.rules_sidebar_generation {
            return;
        }
        let Some(state) = self.rules_sidebar.as_mut() else {
            return;
        };
        state.finish_load(result);
        tui.frame_requester().schedule_frame();
        // 下一轮只在本轮完成后启动，因此慢 CLI 不会积压重叠请求。
        self.spawn_rules_sidebar_load(RULES_POLL_INTERVAL);
    }

    fn refresh_rules_sidebar_context_if_needed(&mut self) {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            return;
        };
        let project_root = self.config.cwd.to_path_buf();
        let context_changed = self
            .rules_sidebar
            .as_ref()
            .is_some_and(|state| !state.context_matches(thread_id, &project_root));
        if !context_changed {
            return;
        }
        self.rules_sidebar_generation = self.rules_sidebar_generation.wrapping_add(1);
        self.rules_sidebar = Some(RulesSidebarState::new(
            thread_id,
            project_root,
            self.transcript_cells.clone(),
            self.keymap.pager.clone(),
        ));
        self.spawn_rules_sidebar_load(Duration::ZERO);
    }

    fn handle_rules_sidebar_key(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) -> bool {
        // 官方输入层级中 popup/modal 拥有当前焦点，全局视图快捷键也必须暂时让路；
        // 侧栏仅在 composer 的普通输入状态下消费自己的专属绑定。
        if !self.chat_widget.no_modal_or_popup_active() {
            return false;
        }
        if self.keymap.app.toggle_rules_sidebar.is_pressed(key_event) {
            self.close_rules_sidebar(tui);
            return true;
        }
        if self.keymap.app.open_transcript.is_pressed(key_event) {
            self.open_transcript_overlay(tui);
            return true;
        }
        if self.keymap.rules_sidebar.close.is_pressed(key_event) {
            self.close_rules_sidebar(tui);
            return true;
        }
        let Some(state) = self.rules_sidebar.as_mut() else {
            return false;
        };
        if self
            .keymap
            .rules_sidebar
            .transcript_jump_top
            .is_pressed(key_event)
        {
            state.jump_transcript(ScrollDestination::Top);
        } else if self
            .keymap
            .rules_sidebar
            .transcript_jump_bottom
            .is_pressed(key_event)
        {
            state.jump_transcript(ScrollDestination::Bottom);
        } else if self.keymap.rules_sidebar.scroll_up.is_pressed(key_event) {
            state.scroll_up();
        } else if self.keymap.rules_sidebar.scroll_down.is_pressed(key_event) {
            state.scroll_down();
        } else if self.keymap.rules_sidebar.page_up.is_pressed(key_event) {
            state.page_up();
        } else if self.keymap.rules_sidebar.page_down.is_pressed(key_event) {
            state.page_down();
        } else {
            return false;
        }
        tui.frame_requester().schedule_frame();
        true
    }

    fn handle_rules_sidebar_mouse(
        &mut self,
        tui: &mut tui::Tui,
        mouse_event: crossterm::event::MouseEvent,
    ) -> bool {
        let area = tui.terminal.viewport_area;
        let chat_widget = &self.chat_widget;
        let Some(state) = self.rules_sidebar.as_mut() else {
            return false;
        };
        if !state.handle_mouse_scroll(area, chat_widget, mouse_event) {
            return false;
        }
        tui.frame_requester().schedule_frame();
        true
    }

    pub(super) async fn handle_rules_sidebar_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
    ) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                if !self.handle_rules_sidebar_key(tui, key_event) {
                    self.handle_key_event(tui, app_server, key_event).await;
                }
            }
            TuiEvent::Mouse(mouse_event) => {
                if !self.handle_rules_sidebar_mouse(tui, mouse_event) {
                    self.chat_widget
                        .handle_mouse_event(tui.terminal.viewport_area, mouse_event);
                }
            }
            TuiEvent::Paste(pasted) => {
                self.chat_widget.handle_paste(pasted.replace('\r', "\n"));
            }
            TuiEvent::Draw | TuiEvent::Resume | TuiEvent::Resize(_) => {
                self.refresh_rules_sidebar_context_if_needed();
                self.chat_widget.maybe_post_pending_notification(tui);
                if self
                    .chat_widget
                    .handle_paste_burst_tick(tui.frame_requester())
                {
                    return Ok(());
                }
                self.chat_widget.pre_draw_tick();
                self.render_rules_sidebar_frame(tui)?;
            }
        }
        Ok(())
    }

    fn render_rules_sidebar_frame(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let active_key = self.chat_widget.active_cell_transcript_key();
        let transcript_cells = &self.transcript_cells;
        let chat_widget = &self.chat_widget;
        let Some(state) = self.rules_sidebar.as_mut() else {
            return Ok(());
        };
        state.sync_transcript_cells(transcript_cells);
        tui.draw(u16::MAX, |frame| {
            if let Some(cursor) = state.render(frame.area(), frame.buffer, chat_widget, active_key)
            {
                frame.set_cursor_style(chat_widget.rules_sidebar_cursor_style(cursor.area));
                frame.set_cursor_position(cursor.position);
            }
        })?;
        if active_key.is_some_and(|key| key.animation_tick.is_some())
            && state.transcript_follows_bottom()
        {
            tui.frame_requester()
                .schedule_frame_in(Duration::from_millis(50));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "rules_sidebar_tests.rs"]
mod tests;
