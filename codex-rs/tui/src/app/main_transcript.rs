//! Temporary transcript viewport used by Shift+wheel in the main chat surface.
//!
//! The native inline viewport remains the default because it provides terminal-owned scrollback
//! and selection. Holding Shift temporarily moves only the transcript portion into an alternate
//! screen while the composer stays owned by the main `ChatWidget`.

use std::sync::Arc;

use super::App;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::ActiveCellTranscriptKey;
use crate::chatwidget::ChatWidget;
use crate::history_cell::HistoryCell;
use crate::keymap::PagerKeymap;
use crate::pager_overlay::ScrollDirection;
use crate::pager_overlay::TranscriptHistoryState;
use crate::pager_overlay::TranscriptOverlay;
use crate::tui;
use color_eyre::eyre::Result;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

pub(crate) const LINES_PER_MOUSE_SCROLL: usize = 3;

#[derive(Clone, Copy)]
struct MainTranscriptLayout {
    timeline: Rect,
    composer: Rect,
}

pub(crate) struct MainTranscriptCursor {
    pub(crate) position: (u16, u16),
    pub(crate) area: Rect,
}

pub(crate) struct MainTranscriptViewport {
    transcript: TranscriptOverlay,
}

impl MainTranscriptViewport {
    pub(crate) fn new(
        transcript_cells: Vec<Arc<dyn HistoryCell>>,
        pager_keymap: PagerKeymap,
    ) -> Self {
        Self {
            transcript: TranscriptOverlay::new(transcript_cells, pager_keymap),
        }
    }

    pub(crate) fn set_history_state(&mut self, state: TranscriptHistoryState) {
        self.transcript.set_history_state(state);
    }

    pub(crate) fn insert_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        self.transcript.insert_cell(cell);
    }

    pub(crate) fn replace_cells(&mut self, cells: Vec<Arc<dyn HistoryCell>>) {
        self.transcript.replace_cells(cells);
    }

    pub(crate) fn consolidate_cells(
        &mut self,
        range: std::ops::Range<usize>,
        consolidated: Arc<dyn HistoryCell>,
    ) {
        self.transcript.consolidate_cells(range, consolidated);
    }

    pub(crate) fn prepend(&mut self, cells: Vec<Arc<dyn HistoryCell>>, width: u16) -> usize {
        self.transcript.prepend(cells, width)
    }

    pub(crate) fn scroll(
        &mut self,
        area: Rect,
        chat_widget: &ChatWidget,
        direction: ScrollDirection,
        active_key: Option<ActiveCellTranscriptKey>,
    ) {
        let layout = Self::layout(area, chat_widget);
        let width = transcript_width(layout.timeline);
        self.transcript.sync_live_tail(width, active_key, |width| {
            chat_widget.active_cell_transcript_hyperlink_lines(width)
        });
        self.transcript.scroll_lines(
            direction,
            LINES_PER_MOUSE_SCROLL,
            width,
            layout.timeline.height,
        );
    }

    pub(crate) fn is_scrolled_to_top(&self, area: Rect, chat_widget: &ChatWidget) -> bool {
        let layout = Self::layout(area, chat_widget);
        self.transcript
            .is_scrolled_to_top(transcript_width(layout.timeline), layout.timeline.height)
    }

    pub(crate) fn is_scrolled_to_bottom(&self) -> bool {
        self.transcript.is_scrolled_to_bottom()
    }

    pub(crate) fn render(
        &mut self,
        area: Rect,
        chat_widget: &ChatWidget,
        active_key: Option<ActiveCellTranscriptKey>,
        buf: &mut Buffer,
    ) -> Option<MainTranscriptCursor> {
        Clear.render(area, buf);
        let layout = Self::layout(area, chat_widget);
        let width = transcript_width(layout.timeline);
        self.transcript.sync_live_tail(width, active_key, |width| {
            chat_widget.active_cell_transcript_hyperlink_lines(width)
        });
        self.transcript.render_timeline(layout.timeline, buf);
        chat_widget.render_main_transcript_bottom_pane(layout.composer, buf);

        chat_widget
            .main_transcript_cursor_pos(layout.composer)
            .map(|position| MainTranscriptCursor {
                position,
                area: layout.composer,
            })
    }

    fn layout(area: Rect, chat_widget: &ChatWidget) -> MainTranscriptLayout {
        let composer_height = chat_widget
            .main_transcript_bottom_pane_height(area.width)
            .min(area.height);
        let separator_height = u16::from(composer_height > 0 && composer_height < area.height);
        let occupied_height = composer_height
            .saturating_add(separator_height)
            .min(area.height);
        let composer_top = area.bottom().saturating_sub(occupied_height);
        let timeline = Rect::new(
            area.x,
            area.y,
            area.width,
            composer_top.saturating_sub(area.y),
        );
        let composer = Rect::new(
            area.x,
            composer_top.saturating_add(separator_height),
            area.width,
            area.bottom()
                .saturating_sub(composer_top.saturating_add(separator_height)),
        );
        MainTranscriptLayout { timeline, composer }
    }
}

fn transcript_width(timeline: Rect) -> u16 {
    // `render_timeline` permanently reserves one scrollbar column, so scrolling must use the same
    // content width as rendering or wrapped cells will produce an incorrect max_scroll.
    timeline.width.saturating_sub(1).max(1)
}

impl App {
    pub(super) fn main_transcript_viewport_active(&self) -> bool {
        self.main_transcript.is_some()
    }

    pub(super) fn open_main_transcript_viewport(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if self.main_transcript.is_some()
            || self.overlay.is_some()
            || self.rules_sidebar.is_some()
            || !self.chat_widget.no_modal_or_popup_active()
        {
            return Ok(());
        }

        tui.enter_alt_screen_without_alternate_scroll()?;
        let mut viewport =
            MainTranscriptViewport::new(self.transcript_cells.clone(), self.keymap.pager.clone());
        if self.scrollback_has_older_history {
            viewport.set_history_state(TranscriptHistoryState::Partial);
        }
        self.main_transcript = Some(viewport);
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) fn close_main_transcript_viewport(&mut self, tui: &mut tui::Tui) {
        if self.main_transcript.take().is_none() {
            return;
        }

        let _ = tui.leave_alt_screen();
        // 退出临时视口后恢复主 composer 的 Ctrl/Shift 按住捕获策略，并把期间延迟的历史行
        // 写回原生 scrollback；否则下一帧会继续停留在 alternate screen 的空白缓冲区。
        let _ = tui.enable_mouse_capture();
        if !self.deferred_history_lines.is_empty() {
            let lines = std::mem::take(&mut self.deferred_history_lines);
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                lines,
                self.history_line_wrap_policy(),
            );
        }
        if self.pending_thread_usage_history_refresh
            && let Err(err) = self.refresh_thread_usage_history_tail(tui)
        {
            tracing::warn!(error = %err, "failed to refresh thread usage after closing main transcript");
        }
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn handle_main_transcript_mouse(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: MouseEvent,
    ) -> Result<bool> {
        let direction = match event.kind {
            MouseEventKind::ScrollUp => ScrollDirection::Up,
            MouseEventKind::ScrollDown => ScrollDirection::Down,
            _ => return Ok(false),
        };
        if !event.modifiers.contains(KeyModifiers::SHIFT) && !tui.shift_is_pressed() {
            return Ok(false);
        }

        self.open_main_transcript_viewport(tui)?;
        let active_key = self.chat_widget.active_cell_transcript_key();
        let area = tui.terminal.viewport_area;
        let chat_widget = &self.chat_widget;
        let scrolled_to_top = {
            let Some(viewport) = self.main_transcript.as_mut() else {
                return Ok(false);
            };
            viewport.scroll(area, chat_widget, direction, active_key);
            viewport.is_scrolled_to_top(area, chat_widget)
        };

        if direction == ScrollDirection::Up
            && scrolled_to_top
            && let Some(thread_id) = self.chat_widget.thread_id()
            && app_server.has_older_history(thread_id)
            && self.request_older_history_page(app_server, thread_id)
            && let Some(viewport) = self.main_transcript.as_mut()
        {
            viewport.set_history_state(TranscriptHistoryState::LoadingOlder);
        }
        tui.frame_requester().schedule_frame();
        Ok(true)
    }

    pub(super) fn render_main_transcript_frame(
        &mut self,
        tui: &mut tui::Tui,
        _screen_size: Size,
    ) -> Result<Rect> {
        let active_key = self.chat_widget.active_cell_transcript_key();
        let chat_widget = &self.chat_widget;
        let Some(viewport) = self.main_transcript.as_mut() else {
            return Ok(Rect::default());
        };
        let mut rendered_area = Rect::default();
        tui.draw(u16::MAX, |frame| {
            rendered_area = frame.area();
            if let Some(cursor) =
                viewport.render(frame.area(), chat_widget, active_key, frame.buffer)
            {
                frame.set_cursor_style(chat_widget.main_transcript_cursor_style(cursor.area));
                frame.set_cursor_position(cursor.position);
            }
            chat_widget.note_rendered_width(frame.area().width);
        })?;
        if active_key.is_some_and(|key| key.animation_tick.is_some())
            && viewport.is_scrolled_to_bottom()
        {
            tui.frame_requester()
                .schedule_frame_in(std::time::Duration::from_millis(50));
        }
        Ok(rendered_area)
    }

    pub(super) fn sync_main_transcript_inserted_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        if let Some(viewport) = self.main_transcript.as_mut() {
            viewport.insert_cell(cell);
        }
    }

    pub(super) fn sync_main_transcript_replaced_cells(&mut self) {
        if let Some(viewport) = self.main_transcript.as_mut() {
            viewport.replace_cells(self.transcript_cells.clone());
        }
    }

    pub(super) fn sync_main_transcript_consolidated_cell(
        &mut self,
        range: std::ops::Range<usize>,
        consolidated: Arc<dyn HistoryCell>,
    ) {
        if let Some(viewport) = self.main_transcript.as_mut() {
            viewport.consolidate_cells(range, consolidated);
        }
    }

    pub(super) fn sync_main_transcript_prepended_cells(
        &mut self,
        cells: Vec<Arc<dyn HistoryCell>>,
        width: u16,
    ) -> usize {
        self.main_transcript
            .as_mut()
            .map_or(0, |viewport| viewport.prepend(cells, width))
    }

    pub(super) fn sync_main_transcript_history_state(&mut self, state: TranscriptHistoryState) {
        if let Some(viewport) = self.main_transcript.as_mut() {
            viewport.set_history_state(state);
        }
    }
}

#[cfg(test)]
#[path = "main_transcript_tests.rs"]
mod tests;
