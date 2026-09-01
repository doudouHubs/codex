//! 规则侧栏中的执行计划面板及其独立滚动状态。

use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Scrollbar;
use ratatui::widgets::ScrollbarOrientation;
use ratatui::widgets::ScrollbarState;
use ratatui::widgets::StatefulWidget;
use ratatui::widgets::Widget;

use crate::history_cell::plan_update_checklist_lines;

#[derive(Default)]
pub(crate) struct RulesSidebarPlanState {
    scroll_offset: usize,
    max_scroll: usize,
    page_height: usize,
}

impl RulesSidebarPlanState {
    pub(crate) fn reset(&mut self) {
        self.scroll_offset = 0;
        self.max_scroll = 0;
        self.page_height = 1;
    }

    pub(crate) fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub(crate) fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.max_scroll);
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer, plan: &UpdatePlanArgs) {
        Clear.render(area, buf);
        if area.is_empty() {
            return;
        }

        let inner = Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(2),
            area.height,
        );
        let completed = plan
            .plan
            .iter()
            .filter(|item| matches!(item.status, StepStatus::Completed))
            .count();
        let header = format!("Plan  {completed}/{}", plan.plan.len());
        Paragraph::new(header.bold()).render(Rect::new(inner.x, inner.y, inner.width, 1), buf);

        let content_y = inner.y.saturating_add(2);
        let content_height = inner.height.saturating_sub(3);
        // 预留滚动条列，计划更新或滚动时正文宽度保持稳定，避免 checkbox 文本跳动。
        let content_area = Rect::new(
            inner.x,
            content_y,
            inner.width.saturating_sub(1),
            content_height,
        );
        let lines = plan_update_checklist_lines(
            plan.explanation.as_deref(),
            &plan.plan,
            content_area.width,
        );
        let line_count = lines.len();
        self.page_height = usize::from(content_area.height);
        self.max_scroll = line_count.saturating_sub(self.page_height);
        self.scroll_offset = self.scroll_offset.min(self.max_scroll);
        Paragraph::new(lines)
            .scroll((u16::try_from(self.scroll_offset).unwrap_or(u16::MAX), 0))
            .render(content_area, buf);

        if self.max_scroll > 0 {
            let scrollbar_area =
                Rect::new(content_area.right(), content_area.y, 1, content_area.height);
            let mut scrollbar_state = ScrollbarState::new(line_count)
                .position(self.scroll_offset)
                .viewport_content_length(self.page_height);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("┃")
                .track_symbol(Some("│"))
                .track_style(ratatui::style::Style::default().dim())
                .begin_symbol(None)
                .end_symbol(None)
                .render(scrollbar_area, buf, &mut scrollbar_state);
        }

        if inner.height > 1 {
            let footer = if self.max_scroll == 0 {
                ""
            } else {
                "Mouse wheel scroll"
            };
            Paragraph::new(footer.dim()).render(
                Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                buf,
            );
        }
    }
}
