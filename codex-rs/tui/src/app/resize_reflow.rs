//! Connects terminal resize events to source-backed transcript scrollback rebuilds.
//!
//! The app stores conversation history as `HistoryCell`s, but it also writes finalized history into
//! terminal scrollback for the normal chat view. When the terminal width changes, this module uses
//! the stored cells as source, clears the Codex-owned terminal history, and re-emits the transcript
//! for the new terminal size.
//!
//! Streaming output is the fragile part of this lifecycle. Active streams first appear as transient
//! stream cells, then consolidate into source-backed finalized cells. Resize work that happens
//! before consolidation is marked as stream-time work so consolidation can force one final rebuild
//! from the finalized source.
//!
//! The row cap is enforced while rendering from `HistoryCell` source, not after writing to the
//! terminal. Initial resume replay uses the same display-line buffering contract so large sessions
//! do not write more retained rows than resize replay would later be willing to rebuild.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use color_eyre::eyre::Result;
use ratatui::layout::Size;
use ratatui::style::Stylize;
use ratatui::text::Line;

use super::App;
use super::InitialHistoryReplayBuffer;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::transcript_reflow::TRANSCRIPT_REFLOW_DEBOUNCE;
use crate::tui;

/// Full terminal width before transcript-specific layout reservations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TerminalWidth(u16);

impl From<ratatui::layout::Size> for TerminalWidth {
    fn from(size: ratatui::layout::Size) -> Self {
        Self(size.width)
    }
}

struct ReflowCellDisplay {
    lines: Vec<HyperlinkLine>,
    is_stream_continuation: bool,
}

/// Rendered transcript lines ready to be replayed into terminal scrollback.
///
/// This is intentionally line-oriented rather than cell-oriented because the terminal only accepts
/// already-wrapped rows. Callers should keep treating `transcript_cells` as the source of truth; the
/// rows here are a transient render product for a single terminal width.
pub(super) struct ReflowRenderResult {
    pub(super) lines: Vec<HyperlinkLine>,
}

pub(super) fn trailing_run_start<T: 'static>(transcript_cells: &[Arc<dyn HistoryCell>]) -> usize {
    let end = transcript_cells.len();
    let mut start = end;

    while start > 0
        && transcript_cells[start - 1].is_stream_continuation()
        && transcript_cells[start - 1].as_any().is::<T>()
    {
        start -= 1;
    }

    if start > 0
        && transcript_cells[start - 1].as_any().is::<T>()
        && !transcript_cells[start - 1].is_stream_continuation()
    {
        start -= 1;
    }

    start
}

impl App {
    pub(super) fn reset_history_emission_state(&mut self) {
        self.has_emitted_history_lines = false;
        self.deferred_history_lines.clear();
        self.last_rendered_history_tail = None;
        self.reset_history_turn_state();
    }

    pub(super) fn reset_history_turn_state(&mut self) {
        self.latest_history_turn_start = None;
        self.latest_history_turn_id = None;
    }

    /// 记录服务端 turn 对应的首个用户 cell，回流时用它恢复完整的最新 turn。
    ///
    /// 标记事件在用户 cell 入 transcript 后才发送，因此这里可以直接从尾部定位刚提交的
    /// 用户 cell；同一个 turn 的 steer 消息再次到达时只绑定 turn id，不会把起点推到后面。
    pub(super) fn mark_history_turn_start(&mut self, turn_id: Option<String>) {
        let Some(start_cell) = self
            .transcript_cells
            .iter()
            .rev()
            .find(|cell| cell.as_any().is::<history_cell::UserHistoryCell>())
            .cloned()
        else {
            return;
        };

        let boundary_is_present = self
            .latest_history_turn_start
            .as_ref()
            .is_some_and(|boundary| {
                self.transcript_cells
                    .iter()
                    .any(|cell| Arc::ptr_eq(cell, boundary))
            });
        if boundary_is_present && self.latest_history_turn_id.as_ref() == turn_id.as_ref() {
            return;
        }

        // 乐观用户消息先以 None 标记；服务端 echo 到达后只补上真实 id，保留原始 turn 起点。
        if boundary_is_present && self.latest_history_turn_id.is_none() && turn_id.is_some() {
            self.latest_history_turn_id = turn_id;
            return;
        }

        self.latest_history_turn_start = Some(start_cell);
        self.latest_history_turn_id = turn_id;
    }

    fn latest_history_turn_start_index(&self) -> Option<usize> {
        self.latest_history_turn_start
            .as_ref()
            .and_then(|boundary| {
                self.transcript_cells
                    .iter()
                    .position(|cell| Arc::ptr_eq(cell, boundary))
            })
            .or_else(|| {
                self.transcript_cells
                    .iter()
                    .rposition(|cell| cell.as_any().is::<history_cell::UserHistoryCell>())
            })
    }

    fn refresh_last_rendered_history_tail(&mut self, width: u16) {
        self.last_rendered_history_tail = if self.overlay.is_none() {
            self.transcript_cells.iter().rev().find_map(|cell| {
                let lines = cell.display_hyperlink_lines_for_mode(
                    width,
                    self.chat_widget.history_render_mode(),
                );
                (!lines.is_empty()).then(|| super::history_ui::RenderedHistoryTail {
                    cell: Arc::downgrade(cell),
                    lines,
                })
            })
        } else {
            None
        };
    }

    fn display_lines_for_history_insert(
        &mut self,
        cell: &dyn HistoryCell,
        width: u16,
    ) -> Vec<HyperlinkLine> {
        let mut display =
            cell.display_hyperlink_lines_for_mode(width, self.chat_widget.history_render_mode());
        if !display.is_empty() && !cell.is_stream_continuation() {
            if self.has_emitted_history_lines {
                display.insert(/*index*/ 0, HyperlinkLine::new(Line::from("")));
            } else {
                self.has_emitted_history_lines = true;
            }
        }
        display
    }

    pub(super) fn insert_history_cell_lines(
        &mut self,
        tui: &mut tui::Tui,
        cell: &dyn HistoryCell,
        width: u16,
    ) {
        let display = self.display_lines_for_history_insert(cell, width);
        if display.is_empty() {
            return;
        }
        if self.alt_screen_surface_active() {
            self.deferred_history_lines.extend(display);
        } else {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                display,
                self.history_line_wrap_policy(),
            );
        }
    }

    /// Start a replay transaction before replayed cells are written to terminal scrollback.
    ///
    /// The transcript remains the complete source of truth. The replay buffer only suppresses
    /// incremental main-screen writes so the end of replay can render one latest-turn snapshot.
    pub(super) fn begin_initial_history_replay_buffer(&mut self) {
        if !self.alt_screen_surface_active() {
            self.initial_history_replay_buffer = Some(InitialHistoryReplayBuffer);
        }
    }

    /// Flush one latest-turn render after replay has populated the complete transcript.
    pub(super) fn finish_initial_history_replay_buffer(&mut self, tui: &mut tui::Tui) {
        if self.initial_history_replay_buffer.take().is_none() {
            return;
        }

        let width = self
            .chat_widget
            .history_wrap_width(tui.terminal.last_known_screen_size.width);
        let reflowed_lines = self.render_transcript_lines_for_reflow(width).lines;

        // 统一在 replay 完成后提交清屏，保证清屏和最新 turn 的首帧不会被中间帧拆开。
        tui.defer_scrollback_clear();
        tui.clear_pending_history_lines();
        self.deferred_history_lines.clear();
        self.transcript_reflow.clear_pending_reflow();
        self.transcript_reflow
            .mark_reflowed_width(tui.terminal.last_known_screen_size.width);
        if !reflowed_lines.is_empty() {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                reflowed_lines,
                self.history_line_wrap_policy(),
            );
        }
        self.refresh_last_rendered_history_tail(width);
        self.refresh_thread_usage_history_cache(width);
        // 最新 turn 的重建已经包含当前 status cell；此时不能再对尚未提交的旧终端尾部
        // 做增量替换，否则下一次 draw 的整批清屏会覆盖这次替换并产生重复行。
        self.pending_thread_usage_history_refresh = false;
    }

    pub(crate) fn history_line_wrap_policy(&self) -> HistoryLineWrapPolicy {
        if self.chat_widget.raw_output_mode() {
            HistoryLineWrapPolicy::Terminal
        } else {
            HistoryLineWrapPolicy::PreWrap
        }
    }

    fn schedule_resize_reflow(&mut self, target_width: Option<u16>) -> bool {
        self.transcript_reflow.schedule_debounced(target_width)
    }

    fn resize_reflow_max_rows(&self) -> Option<usize> {
        crate::resize_reflow_cap::resize_reflow_max_rows(self.config.terminal_resize_reflow)
    }

    pub(super) fn update_visible_history_rows(&mut self, screen_size: Size) {
        let width = screen_size.width.max(/*other*/ 1);
        let viewport_height = self
            .with_chat_widget_frame(width, |desired_height, _| desired_height)
            .min(screen_size.height);
        self.transcript_reflow.set_visible_history_rows(
            screen_size
                .height
                .saturating_sub(viewport_height)
                .max(/*other*/ 1),
        );
    }

    fn clear_terminal_for_resize_replay(&mut self, tui: &mut tui::Tui) -> Result<()> {
        // 清屏必须延迟到 draw_with_resize_reflow 的同步批次，避免清屏后等待回放重建时出现空白帧。
        tui.defer_scrollback_clear();
        Ok(())
    }

    fn defer_initial_replay_reflow(&mut self) -> bool {
        if self.initial_history_replay_buffer.is_some() {
            // replay 尚未结束时不能清屏重建，否则会把半成品 turn 暴露出来；结束事件统一
            // 从完整 transcript 定位最后一个用户消息后再绘制。
            self.transcript_reflow.clear_stream_flags();
            return true;
        }
        false
    }

    /// Finish stream consolidation by repairing any resize work that happened during streaming.
    ///
    /// This is called after agent-message stream cells have either been replaced by an
    /// `AgentMarkdownCell` or found to need no replacement. If a resize happened while the stream
    /// was active or while its transient cells were still present, this method runs an immediate
    /// source-backed reflow so terminal scrollback reflects the finalized cell instead of the
    /// transient stream rows.
    pub(super) fn maybe_finish_stream_reflow(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if self.transcript_reflow.take_stream_finish_reflow_needed() {
            if self.defer_initial_replay_reflow() {
                return Ok(());
            }
            self.schedule_immediate_resize_reflow(tui);
            let screen_size = tui.terminal.last_known_screen_size;
            self.maybe_run_resize_reflow(tui, screen_size)?;
        } else if self.transcript_reflow.pending_is_due(Instant::now()) {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    pub(super) fn schedule_immediate_resize_reflow(&mut self, tui: &mut tui::Tui) {
        self.transcript_reflow.schedule_immediate();
        tui.frame_requester().schedule_frame();
    }

    /// Force stream-finalized output through the resize reflow path.
    ///
    /// Proposed plan consolidation uses this stricter path because a completed plan is inserted or
    /// replaced as one styled source-backed cell. If this reflow is skipped after a stream-time
    /// resize, the visible scrollback can keep the pre-consolidation wrapping.
    pub(super) fn finish_required_stream_reflow(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if self.defer_initial_replay_reflow() {
            return Ok(());
        }

        self.schedule_immediate_resize_reflow(tui);
        let screen_size = tui.terminal.last_known_screen_size;
        self.maybe_run_resize_reflow(tui, screen_size)?;
        if !self.transcript_reflow.has_pending_reflow() {
            self.transcript_reflow.clear_stream_flags();
        }
        Ok(())
    }

    /// Record terminal size changes and schedule any resize-sensitive transcript work.
    ///
    /// Width changes need a rebuild because transcript wrapping changes. Height changes can expose,
    /// hide, or shift rows around the inline viewport, so they also rebuild from source-backed
    /// cells. The first observed width initializes resize tracking without scheduling a rebuild,
    /// because there is no previously emitted width to repair yet.
    pub(super) fn handle_draw_size_change(
        &mut self,
        size: ratatui::layout::Size,
        last_known_screen_size: ratatui::layout::Size,
        frame_requester: &tui::FrameRequester,
    ) -> bool {
        if size != last_known_screen_size || self.transcript_reflow.visible_history_rows().is_none()
        {
            self.update_visible_history_rows(size);
        }
        let width = self.transcript_reflow.note_width(size.width);
        let reflow_needed = self.transcript_reflow.reflow_needed_for_width(size.width);
        let height_changed = size.height != last_known_screen_size.height;
        let should_rebuild_transcript = reflow_needed || height_changed;
        if width.changed || width.initialized {
            self.chat_widget.on_terminal_resize(size.width);
        }
        if should_rebuild_transcript {
            if reflow_needed && self.should_mark_reflow_as_stream_time() {
                self.transcript_reflow.mark_resize_requested_during_stream();
            }
            let target_width = reflow_needed.then_some(size.width);
            if self.schedule_resize_reflow(target_width) {
                frame_requester.schedule_frame();
            } else {
                frame_requester.schedule_frame_in(TRANSCRIPT_REFLOW_DEBOUNCE);
            }
        }
        if size != last_known_screen_size {
            self.refresh_status_line();
        }
        self.maybe_clear_resize_reflow_without_terminal();
        should_rebuild_transcript
    }

    fn maybe_clear_resize_reflow_without_terminal(&mut self) {
        let Some(deadline) = self.transcript_reflow.pending_until() else {
            return;
        };
        if Instant::now() < deadline
            || self.alt_screen_surface_active()
            || !self.transcript_cells.is_empty()
        {
            return;
        }

        self.transcript_reflow.clear_pending_reflow();
        self.reset_history_emission_state();
    }

    pub(super) fn handle_draw_pre_render(
        &mut self,
        tui: &mut tui::Tui,
        size: ratatui::layout::Size,
    ) -> Result<()> {
        let should_rebuild_transcript = self.handle_draw_size_change(
            size,
            tui.terminal.last_known_screen_size,
            &tui.frame_requester(),
        );
        if should_rebuild_transcript && !self.alt_screen_surface_active() {
            // Resize-sensitive history inserts queued before this frame may be wrapped for the old
            // viewport or targeted at rows no longer visible. Drop them and let resize reflow
            // rebuild from transcript cells.
            tui.clear_pending_history_lines();
            // 如果上一次 replay 刚排队了清屏但还没绘制，resize 会重新生成一批行；撤销旧
            // 提交标记可以让旧画面继续保留到新的 reflow 完成，避免出现空白间隔。
            tui.cancel_deferred_scrollback_clear();
        }
        self.maybe_run_resize_reflow(tui, size)?;
        Ok(())
    }

    /// Run a pending transcript reflow when its debounce deadline has arrived.
    ///
    /// Reflow is deferred while an overlay is active because the overlay owns the current draw
    /// surface. Callers must keep using `HistoryCell` source as the rebuild input; attempting to
    /// reuse terminal-wrapped output here would preserve exactly the stale wrapping this feature is
    /// meant to remove.
    pub(super) fn maybe_run_resize_reflow(
        &mut self,
        tui: &mut tui::Tui,
        screen_size: ratatui::layout::Size,
    ) -> Result<()> {
        let Some(deadline) = self.transcript_reflow.pending_until() else {
            return Ok(());
        };
        let now = Instant::now();
        if now < deadline {
            // Later resize events push the reflow deadline out, while the frame scheduler coalesces
            // delayed draws to the earliest requested instant. If an early draw arrives before the
            // latest quiet-period deadline, re-arm the draw so the pending reflow cannot get stuck
            // until the next keypress.
            tui.frame_requester().schedule_frame_in(deadline - now);
            return Ok(());
        }
        if self.alt_screen_surface_active() {
            return Ok(());
        }
        if self.initial_history_replay_buffer.is_some() {
            // 回放事务必须先完成；否则 resize 会把尚未完成的 transcript 清屏重放到主屏。
            return Ok(());
        }

        self.transcript_reflow.clear_pending_reflow();

        // Track that a reflow happened during an active stream or while trailing
        // unconsolidated AgentMessageCells are still pending consolidation so
        // ConsolidateAgentMessage can schedule a follow-up reflow.
        let reflow_ran_during_stream =
            !self.transcript_cells.is_empty() && self.should_mark_reflow_as_stream_time();

        let width = self.reflow_transcript_now(tui, screen_size.into())?;
        self.transcript_reflow.mark_reflowed_width(width.0);

        if reflow_ran_during_stream {
            self.transcript_reflow.mark_ran_during_stream();
        }
        // Some terminals settle their final reported width after the repaint that handled the
        // last resize event. Request one cheap follow-up draw so `handle_draw_pre_render` can
        // sample that width and schedule a final reflow if needed.
        tui.schedule_screen_size_recheck(TRANSCRIPT_REFLOW_DEBOUNCE);

        Ok(())
    }

    pub(super) fn reflow_transcript_now(
        &mut self,
        tui: &mut tui::Tui,
        terminal_width: TerminalWidth,
    ) -> Result<TerminalWidth> {
        let width = self.chat_widget.history_wrap_width(terminal_width.0);
        if self.transcript_cells.is_empty() {
            // Drop any queued pre-resize/pre-consolidation inserts before rebuilding from cells.
            tui.clear_pending_history_lines();
            self.reset_history_emission_state();
            return Ok(terminal_width);
        }

        // 主屏只重建最新 turn，但分页是否需要继续补齐仍应基于完整 transcript 的已加载行数。
        // 否则一个很短的最新 turn 会让每次 resize 都误判为 scrollback 未填满。
        let loaded_scrollback_rows = self.rendered_transcript_rows_for_scrollback(width);
        let reflow_result = self.render_transcript_lines_for_reflow(width);
        let reflowed_lines = reflow_result.lines;

        // Drop any queued pre-resize/pre-consolidation inserts before rebuilding from cells.
        tui.clear_pending_history_lines();
        self.clear_terminal_for_resize_replay(tui)?;

        self.deferred_history_lines.clear();
        if !reflowed_lines.is_empty() {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                reflowed_lines,
                self.history_line_wrap_policy(),
            );
        }
        self.refresh_last_rendered_history_tail(width);
        self.refresh_thread_usage_history_cache(width);
        if self.pending_thread_usage_history_refresh {
            // 当前重建已经从 source cell 生成了新的可见尾部，不能在待清屏批次尚未提交时
            // 再通过 replace_visible_history_tail 修改旧终端内容；更新缓存后由后续实时刷新接管。
            self.pending_thread_usage_history_refresh = false;
        }
        self.request_scrollback_history_top_up(loaded_scrollback_rows);

        Ok(terminal_width)
    }

    /// Return the loaded-row count needed to decide whether scrollback needs another page.
    ///
    /// This count is intentionally independent from `render_transcript_lines_for_reflow`: the
    /// latter is the main-screen projection and deliberately starts at the latest user turn, while
    /// pagination needs to know whether the in-memory source has filled the configured history
    /// budget before deciding to fetch another older page. The reverse walk stops at that budget,
    /// so a large transcript does not pay for a full markdown render just to answer this question.
    pub(super) fn rendered_transcript_rows_for_scrollback(&self, width: u16) -> usize {
        let Some(max_rows) = self.resize_reflow_max_rows() else {
            return 0;
        };
        let mut rows = 0usize;
        let mut has_emitted_history_lines = false;
        for cell in self.transcript_cells.iter().rev() {
            let display = cell
                .display_hyperlink_lines_for_mode(width, self.chat_widget.history_render_mode());
            if !display.is_empty() && !cell.is_stream_continuation() {
                if has_emitted_history_lines {
                    rows += 1;
                } else {
                    has_emitted_history_lines = true;
                }
            }
            rows += display.len();
            if rows >= max_rows {
                return max_rows;
            }
        }
        rows
    }

    /// Return whether older paginated source can fill unused configured scrollback rows.
    pub(super) fn scrollback_history_needs_top_up(&self, rendered_rows: usize) -> bool {
        self.overlay.is_none()
            && self.scrollback_has_older_history
            && self
                .resize_reflow_max_rows()
                .is_some_and(|max_rows| rendered_rows < max_rows)
    }

    fn request_scrollback_history_top_up(&self, rendered_rows: usize) {
        if self.scrollback_history_needs_top_up(rendered_rows)
            && let Some(thread_id) = self.chat_widget.thread_id()
        {
            tracing::debug!(
                %thread_id,
                rendered_rows,
                max_rows = self.resize_reflow_max_rows(),
                "refilling underfilled terminal scrollback from paginated history"
            );
            self.app_event_tx
                .send(crate::app_event::AppEvent::RequestOlderScrollbackHistory { thread_id });
        }
    }

    /// Rebuild scrollback after rollback removes transcript cells.
    ///
    /// Unlike resize reflow, rollback must clear the terminal even when no cells remain. Otherwise
    /// the cancelled user prompt stays visible in scrollback despite being removed from the source
    /// transcript.
    pub(super) fn rebuild_transcript_after_backtrack(
        &mut self,
        tui: &mut tui::Tui,
        terminal_width: TerminalWidth,
    ) -> Result<()> {
        let width = self.chat_widget.history_wrap_width(terminal_width.0);
        let reflowed_lines = if self.transcript_cells.is_empty() {
            self.reset_history_emission_state();
            Vec::new()
        } else {
            self.render_transcript_lines_for_reflow(width).lines
        };

        tui.clear_pending_history_lines();
        self.clear_terminal_for_resize_replay(tui)?;

        self.deferred_history_lines.clear();
        if !reflowed_lines.is_empty() {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                reflowed_lines,
                self.history_line_wrap_policy(),
            );
        }
        self.refresh_last_rendered_history_tail(width);
        self.refresh_thread_usage_history_cache(width);
        self.pending_thread_usage_history_refresh = false;

        Ok(())
    }

    /// Render the history that belongs on the main screen after a clear.
    ///
    /// A `UserHistoryCell` marks the beginning of a turn, so normal rebuilds render that last user
    /// message and every cell after it without applying the terminal row cap. This keeps the main
    /// screen useful while leaving the complete transcript available to the transcript surfaces.
    /// Synthetic histories without a user cell use the previous capped suffix renderer as a safe
    /// fallback because there is no reliable turn boundary to identify.
    pub(super) fn render_transcript_lines_for_reflow(&mut self, width: u16) -> ReflowRenderResult {
        if let Some(start) = self.latest_history_turn_start_index() {
            let has_earlier_user_turn = self.transcript_cells[..start]
                .iter()
                .any(|cell| cell.as_any().is::<history_cell::UserHistoryCell>());
            let mut has_emitted_history_lines = false;
            let mut reflowed_lines = Vec::new();
            for cell in &self.transcript_cells[start..] {
                let display = cell.display_hyperlink_lines_for_mode(
                    width,
                    self.chat_widget.history_render_mode(),
                );
                if !display.is_empty() && !cell.is_stream_continuation() {
                    if has_emitted_history_lines {
                        reflowed_lines.push(HyperlinkLine::new(Line::from("")));
                    } else {
                        has_emitted_history_lines = true;
                    }
                }
                reflowed_lines.extend(display);
            }
            self.prepend_scrollback_history_notice_without_row_cap(
                &mut reflowed_lines,
                has_earlier_user_turn || self.scrollback_has_older_history,
                width,
            );
            self.has_emitted_history_lines = !reflowed_lines.is_empty();
            return ReflowRenderResult {
                lines: reflowed_lines,
            };
        }

        // 没有明确的用户 turn 边界时保留旧的后缀策略，避免清屏后得到空白主屏。
        let row_cap = self.resize_reflow_max_rows();
        let mut cell_displays = VecDeque::new();
        let mut rendered_rows = 0usize;
        let mut start = self.transcript_cells.len();
        let mut history_was_truncated = false;

        while start > 0 {
            start -= 1;
            let cell = self.transcript_cells[start].clone();
            let lines = cell
                .display_hyperlink_lines_for_mode(width, self.chat_widget.history_render_mode());
            rendered_rows += lines.len();
            cell_displays.push_front(ReflowCellDisplay {
                lines,
                is_stream_continuation: cell.is_stream_continuation(),
            });

            if row_cap.is_some_and(|max_rows| rendered_rows > max_rows) {
                history_was_truncated = true;
                break;
            }
        }

        while start > 0
            && cell_displays
                .front()
                .is_some_and(|display| display.is_stream_continuation)
        {
            start -= 1;
            let cell = self.transcript_cells[start].clone();
            cell_displays.push_front(ReflowCellDisplay {
                lines: cell.display_hyperlink_lines_for_mode(
                    width,
                    self.chat_widget.history_render_mode(),
                ),
                is_stream_continuation: cell.is_stream_continuation(),
            });
        }

        let mut has_emitted_history_lines = false;
        let mut reflowed_lines = Vec::new();
        for display in cell_displays {
            if !display.lines.is_empty() && !display.is_stream_continuation {
                if has_emitted_history_lines {
                    reflowed_lines.push(HyperlinkLine::new(Line::from("")));
                } else {
                    has_emitted_history_lines = true;
                }
            }
            reflowed_lines.extend(display.lines);
        }
        if let Some(max_rows) = row_cap
            && reflowed_lines.len() > max_rows
        {
            history_was_truncated = true;
            let trimmed_line_count = reflowed_lines.len() - max_rows;
            reflowed_lines = reflowed_lines.split_off(trimmed_line_count);
        }
        self.prepend_scrollback_history_notice(&mut reflowed_lines, history_was_truncated, width);
        self.has_emitted_history_lines = !reflowed_lines.is_empty();

        ReflowRenderResult {
            lines: reflowed_lines,
        }
    }

    fn prepend_scrollback_history_notice(
        &self,
        lines: &mut Vec<HyperlinkLine>,
        history_was_truncated: bool,
        width: u16,
    ) {
        self.prepend_scrollback_history_notice_with_policy(
            lines,
            history_was_truncated,
            width,
            /*apply_row_cap*/ true,
        );
    }

    fn prepend_scrollback_history_notice_without_row_cap(
        &self,
        lines: &mut Vec<HyperlinkLine>,
        history_was_truncated: bool,
        width: u16,
    ) {
        self.prepend_scrollback_history_notice_with_policy(
            lines,
            history_was_truncated,
            width,
            /*apply_row_cap*/ false,
        );
    }

    fn prepend_scrollback_history_notice_with_policy(
        &self,
        lines: &mut Vec<HyperlinkLine>,
        history_was_truncated: bool,
        width: u16,
        apply_row_cap: bool,
    ) {
        if lines.is_empty() || (!history_was_truncated && !self.scrollback_has_older_history) {
            return;
        }
        let Some(binding) = crate::keymap::primary_binding(&self.keymap.app.open_transcript) else {
            return;
        };
        let notice = Line::from(format!(
            "Earlier messages are available — press {} to view the full transcript",
            binding.display_label()
        ))
        .dim();
        let notice_lines =
            crate::wrapping::word_wrap_lines([notice], usize::from(width.max(/*other*/ 1)));
        if apply_row_cap && let Some(max_rows) = self.resize_reflow_max_rows() {
            let available_history_rows = max_rows.saturating_sub(notice_lines.len());
            if available_history_rows == 0 {
                return;
            }
            if lines.len() > available_history_rows {
                lines.drain(..lines.len() - available_history_rows);
            }
        }
        lines.splice(0..0, notice_lines.into_iter().map(HyperlinkLine::new));
    }

    /// Return whether current transcript state should be treated as stream-time resize state.
    ///
    /// The active stream controllers cover normal streaming. The trailing-cell checks cover the
    /// narrow window after a controller has stopped but before the app has processed the
    /// consolidation event that replaces transient stream cells with source-backed cells.
    pub(super) fn should_mark_reflow_as_stream_time(&self) -> bool {
        self.chat_widget.has_active_agent_stream()
            || self.chat_widget.has_active_plan_stream()
            || trailing_run_start::<history_cell::AgentMessageCell>(&self.transcript_cells)
                < self.transcript_cells.len()
            || trailing_run_start::<history_cell::ProposedPlanStreamCell>(&self.transcript_cells)
                < self.transcript_cells.len()
    }
}

#[cfg(test)]
#[path = "resize_reflow_tests.rs"]
mod tests;
