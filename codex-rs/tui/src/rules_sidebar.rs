use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use codex_protocol::ThreadId;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Scrollbar;
use ratatui::widgets::ScrollbarOrientation;
use ratatui::widgets::ScrollbarState;
use ratatui::widgets::StatefulWidget;
use ratatui::widgets::Widget;
use serde::Deserialize;
use textwrap::Options;
use tokio::process::Command;

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::chatwidget::ChatWidget;
use crate::history_cell::HistoryCell;
use crate::keymap::PagerKeymap;
use crate::pager_overlay::ScrollDestination;
use crate::pager_overlay::ScrollDirection;
use crate::pager_overlay::TranscriptOverlay;
use crate::rules_sidebar_plan::RulesSidebarPlanState;

pub(crate) const RULES_SIDEBAR_MIN_SPLIT_WIDTH: u16 = 112;
const RULES_SIDEBAR_MIN_WIDTH: u16 = 40;
const RULES_SIDEBAR_MAX_WIDTH: u16 = 64;
const RULES_LOAD_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RULES_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RULES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuleItem {
    pub(crate) title: String,
    pub(crate) content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RulesSidebarLoad {
    pub(crate) rules: Vec<RuleItem>,
}

pub(crate) struct RulesSidebarCursor {
    pub(crate) position: (u16, u16),
    pub(crate) area: Rect,
}

#[derive(Clone, Copy)]
struct RulesSidebarLayout {
    timeline: Rect,
    bottom: Rect,
    divider: Rect,
    rules: Rect,
    plan: Option<Rect>,
    plan_divider: Option<Rect>,
}

fn split_sidebar_area(area: Rect) -> (Rect, Rect, Rect) {
    // 计划区至少保留一行；扣除分隔线后的奇数高度多给规则区一行。
    let plan_height = area.height.saturating_sub(1) / 2;
    let divider = Rect::new(
        area.x,
        area.bottom().saturating_sub(plan_height + 1),
        area.width,
        1,
    );
    let rules = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(plan_height + 1),
    );
    let plan = Rect::new(area.x, divider.bottom(), area.width, plan_height);
    (rules, divider, plan)
}

fn rules_sidebar_width(total_width: u16) -> u16 {
    // 侧栏约占终端宽度的三分之一；设置上下限是为了兼顾窄屏主区可用性和宽屏计划可读性。
    (total_width / 3).clamp(RULES_SIDEBAR_MIN_WIDTH, RULES_SIDEBAR_MAX_WIDTH)
}

fn rules_sidebar_layout(area: Rect, chat_widget: &ChatWidget) -> Option<RulesSidebarLayout> {
    if area.width < RULES_SIDEBAR_MIN_SPLIT_WIDTH {
        return None;
    }
    let sidebar_width = rules_sidebar_width(area.width);
    let rules_x = area.right().saturating_sub(sidebar_width);
    let divider = Rect::new(rules_x.saturating_sub(1), area.y, 1, area.height);
    let left = Rect::new(
        area.x,
        area.y,
        divider.x.saturating_sub(area.x),
        area.height,
    );
    let sidebar = Rect::new(rules_x, area.y, sidebar_width, area.height);
    let (rules, plan_divider, plan) = if chat_widget.latest_update_plan().is_some() {
        let (rules, plan_divider, plan) = split_sidebar_area(sidebar);
        (rules, Some(plan_divider), Some(plan))
    } else {
        (sidebar, None, None)
    };
    let bottom_height = chat_widget
        .rules_sidebar_bottom_pane_height(left.width)
        .min(left.height);
    let timeline_height = left.height.saturating_sub(bottom_height);
    let timeline = Rect::new(left.x, left.y, left.width, timeline_height);
    let bottom = Rect::new(
        left.x,
        left.y.saturating_add(timeline_height),
        left.width,
        bottom_height,
    );
    Some(RulesSidebarLayout {
        timeline,
        bottom,
        divider,
        rules,
        plan,
        plan_divider,
    })
}

#[derive(Deserialize)]
struct RuleListOutput {
    session_id: String,
    rule_count: usize,
    rules: Vec<RuleListItem>,
}

#[derive(Deserialize)]
struct RuleListItem {
    title: String,
    content: String,
}

pub(crate) struct RulesSidebarState {
    thread_id: ThreadId,
    project_root: PathBuf,
    transcript: TranscriptOverlay,
    rules: Vec<RuleItem>,
    error: Option<String>,
    loading: bool,
    scroll_offset: usize,
    max_scroll: usize,
    page_height: usize,
    plan: RulesSidebarPlanState,
}

impl RulesSidebarState {
    pub(crate) fn new(
        thread_id: ThreadId,
        project_root: PathBuf,
        transcript_cells: Vec<Arc<dyn HistoryCell>>,
        pager_keymap: PagerKeymap,
    ) -> Self {
        Self {
            thread_id,
            project_root,
            transcript: TranscriptOverlay::new(transcript_cells, pager_keymap),
            rules: Vec::new(),
            error: None,
            loading: false,
            scroll_offset: 0,
            max_scroll: 0,
            page_height: 1,
            plan: RulesSidebarPlanState::default(),
        }
    }

    pub(crate) fn context_matches(&self, thread_id: ThreadId, project_root: &Path) -> bool {
        self.thread_id == thread_id && self.project_root == project_root
    }

    pub(crate) fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub(crate) fn begin_load(&mut self) {
        self.loading = true;
    }

    pub(crate) fn finish_load(&mut self, result: Result<RulesSidebarLoad, String>) {
        self.loading = false;
        match result {
            Ok(load) => {
                self.rules = load.rules;
                self.error = None;
            }
            Err(error) => {
                // 轮询失败时保留上次成功内容，避免瞬时插件故障把用户正在看的规则清空。
                self.error = Some(error);
            }
        }
    }

    pub(crate) fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub(crate) fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.max_scroll);
    }

    pub(crate) fn page_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(self.page_height.max(1));
    }

    pub(crate) fn page_down(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(self.page_height.max(1))
            .min(self.max_scroll);
    }

    pub(crate) fn sync_transcript_cells(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        let unchanged = self.transcript.cells_match(cells);
        if !unchanged {
            self.transcript.replace_cells(cells.to_vec());
        }
    }

    pub(crate) fn transcript_follows_bottom(&self) -> bool {
        self.transcript.is_scrolled_to_bottom()
    }

    pub(crate) fn scroll_transcript(&mut self, direction: ScrollDirection) {
        self.transcript.scroll_line(direction);
    }

    pub(crate) fn jump_transcript(&mut self, destination: ScrollDestination) {
        self.transcript.scroll_to(destination);
    }

    pub(crate) fn scroll_plan(&mut self, direction: ScrollDirection) {
        match direction {
            ScrollDirection::Up => self.plan.scroll_up(),
            ScrollDirection::Down => self.plan.scroll_down(),
        }
    }

    pub(crate) fn handle_mouse_scroll(
        &mut self,
        area: Rect,
        chat_widget: &ChatWidget,
        event: MouseEvent,
    ) -> bool {
        let direction = match event.kind {
            MouseEventKind::ScrollUp => ScrollDirection::Up,
            MouseEventKind::ScrollDown => ScrollDirection::Down,
            _ => return false,
        };
        let position = Position::new(event.column, event.row);
        if let Some(layout) = rules_sidebar_layout(area, chat_widget) {
            if layout.timeline.contains(position) {
                self.scroll_transcript(direction);
                return true;
            }
            if layout.plan.is_some_and(|plan| plan.contains(position)) {
                self.scroll_plan(direction);
                return true;
            }
            return false;
        }
        if chat_widget.latest_update_plan().is_some()
            && split_sidebar_area(area).2.contains(position)
        {
            self.scroll_plan(direction);
            return true;
        }
        false
    }

    pub(crate) fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        chat_widget: &ChatWidget,
        active_key: Option<ActiveCellTranscriptKey>,
    ) -> Option<RulesSidebarCursor> {
        let Some(layout) = rules_sidebar_layout(area, chat_widget) else {
            if let Some(plan) = chat_widget.latest_update_plan() {
                // 窄屏没有左侧 transcript，但规则和执行计划仍保持同样的上下语义。
                let (rules, plan_divider, plan_area) = split_sidebar_area(area);
                self.render_rules(rules, buf);
                Paragraph::new(
                    (0..plan_divider.width)
                        .map(|_| Line::from("─".dim()))
                        .collect::<Vec<_>>(),
                )
                .render(plan_divider, buf);
                self.plan.render(plan_area, buf, plan);
            } else {
                self.plan.reset();
                self.render_rules(area, buf);
            }
            return None;
        };

        self.transcript
            .sync_live_tail(layout.timeline.width.max(1), active_key, |width| {
                chat_widget.active_cell_transcript_hyperlink_lines(width)
            });
        self.transcript.render_timeline(layout.timeline, buf);
        chat_widget.render_rules_sidebar_bottom_pane(layout.bottom, buf);
        Paragraph::new(
            (0..layout.divider.height)
                .map(|_| Line::from("│".dim()))
                .collect::<Vec<_>>(),
        )
        .render(layout.divider, buf);
        self.render_rules(layout.rules, buf);
        if let (Some(plan_area), Some(plan_divider), Some(plan)) = (
            layout.plan,
            layout.plan_divider,
            chat_widget.latest_update_plan(),
        ) {
            Paragraph::new(
                (0..plan_divider.width)
                    .map(|_| Line::from("─".dim()))
                    .collect::<Vec<_>>(),
            )
            .render(plan_divider, buf);
            self.plan.render(plan_area, buf, plan);
        } else {
            self.plan.reset();
        }
        chat_widget
            .rules_sidebar_cursor_pos(layout.bottom)
            .map(|position| RulesSidebarCursor {
                position,
                // 光标形态取决于 composer 实际布局，不能拿整个 frame 的宽度重新计算。
                area: layout.bottom,
            })
    }

    fn render_rules(&mut self, area: Rect, buf: &mut Buffer) {
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
        // 错误详情属于可滚动内容；固定状态放在标题里，保证用户滚动后仍能发现数据已经过期。
        let status = if self.loading {
            "  refreshing"
        } else if self.error.is_some() {
            "  stale"
        } else {
            ""
        };
        let header = format!("Rules  {}{status}", self.rules.len());
        Paragraph::new(header.bold()).render(Rect::new(inner.x, inner.y, inner.width, 1), buf);

        let content_y = inner.y.saturating_add(2);
        let content_height = inner.height.saturating_sub(3);
        // 滚动条列永久预留，轮询加载出更多规则时正文宽度不会突然变化。
        let content_area = Rect::new(
            inner.x,
            content_y,
            inner.width.saturating_sub(1),
            content_height,
        );
        let lines = self.rule_lines(content_area.width);
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
                "Ctrl+T close".dim()
            } else {
                "Ctrl+Alt+Up/Down scroll  Ctrl+T close".dim()
            };
            Paragraph::new(footer).render(
                Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                buf,
            );
        }
    }

    fn rule_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = usize::from(width.max(1));
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            push_wrapped(&mut lines, error, width, ratatui::prelude::Stylize::red);
            if !self.rules.is_empty() {
                lines.push("Showing last loaded rules.".dim().into());
                lines.push("".into());
            }
        }
        if self.rules.is_empty() {
            if self.error.is_none() {
                lines.push(if self.loading {
                    "Loading rules...".dim().into()
                } else {
                    "No rules selected for this session.".dim().into()
                });
            }
            return lines;
        }

        for (index, rule) in self.rules.iter().enumerate() {
            let title = if rule.title.trim().is_empty() {
                format!("{}. Untitled rule", index + 1)
            } else {
                format!("{}. {}", index + 1, rule.title.trim())
            };
            push_wrapped(&mut lines, &title, width, ratatui::prelude::Stylize::bold);
            for paragraph in rule.content.lines() {
                if paragraph.is_empty() {
                    lines.push("".into());
                } else {
                    push_wrapped(&mut lines, paragraph, width, Span::from);
                }
            }
            if index + 1 < self.rules.len() {
                lines.push("".into());
            }
        }
        lines
    }
}

fn push_wrapped(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    style: impl Fn(String) -> Span<'static>,
) {
    let options = Options::new(width.max(1)).break_words(true);
    for line in textwrap::wrap(text, options) {
        lines.push(style(line.into_owned()).into());
    }
}

pub(crate) fn locate_rule_system_binary(skill_path: Option<&Path>) -> Result<PathBuf, String> {
    let skill_path = skill_path
        .ok_or_else(|| "rule-system:rule-list is not enabled for this workspace.".to_string())?;
    let skill_dir = skill_path
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "rule-list"))
        .ok_or_else(|| "The rule-list skill path has an unexpected layout.".to_string())?;
    let skills_dir = skill_dir
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "skills"))
        .ok_or_else(|| "The rule-system skills directory could not be resolved.".to_string())?;
    let plugin_root = skills_dir
        .parent()
        .ok_or_else(|| "The rule-system plugin root could not be resolved.".to_string())?;
    let executable = plugin_root.join("bin").join(if cfg!(windows) {
        "rule-system.exe"
    } else {
        "rule-system"
    });
    executable.is_file().then_some(executable).ok_or_else(|| {
        "The rule-system CLI is unavailable in the enabled plugin version.".to_string()
    })
}

pub(crate) async fn load_rules(
    executable: PathBuf,
    thread_id: ThreadId,
    project_root: PathBuf,
) -> Result<RulesSidebarLoad, String> {
    let mut command = Command::new(&executable);
    command
        .arg("list")
        .arg("--session-id")
        .arg(thread_id.to_string())
        .arg("--project-root")
        .arg(&project_root)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(RULES_LOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| "Timed out while loading rules from rule-system.".to_string())?
        .map_err(|err| format!("Failed to start rule-system: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("rule-system exited with {}.", output.status)
        } else {
            format!("rule-system failed: {detail}")
        });
    }
    if output.stdout.len() > MAX_RULES_OUTPUT_BYTES {
        return Err("rule-system returned more than 1 MiB of rule data.".to_string());
    }
    parse_rules_output(&output.stdout, thread_id)
}

fn parse_rules_output(output: &[u8], thread_id: ThreadId) -> Result<RulesSidebarLoad, String> {
    let response: RuleListOutput = serde_json::from_slice(output)
        .map_err(|err| format!("rule-system returned invalid JSON: {err}"))?;
    if response.session_id != thread_id.to_string() {
        return Err("rule-system returned rules for a different session.".to_string());
    }
    if response.rule_count != response.rules.len() {
        return Err("rule-system returned an inconsistent rule count.".to_string());
    }
    if response.rules.len() > MAX_RULES {
        return Err(format!("rule-system returned more than {MAX_RULES} rules."));
    }
    Ok(RulesSidebarLoad {
        rules: response
            .rules
            .into_iter()
            .map(|rule| RuleItem {
                title: rule.title,
                content: rule.content,
            })
            .collect(),
    })
}

#[cfg(test)]
#[path = "rules_sidebar_tests.rs"]
mod tests;
