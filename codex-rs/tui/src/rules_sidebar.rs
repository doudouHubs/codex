use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use codex_protocol::ThreadId;
use ratatui::buffer::Buffer;
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

pub(crate) const RULES_SIDEBAR_MIN_SPLIT_WIDTH: u16 = 112;
const RULES_SIDEBAR_WIDTH: u16 = 40;
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

    pub(crate) fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        chat_widget: &ChatWidget,
        active_key: Option<ActiveCellTranscriptKey>,
    ) -> Option<RulesSidebarCursor> {
        if area.width < RULES_SIDEBAR_MIN_SPLIT_WIDTH {
            self.render_rules(area, buf);
            return None;
        }

        let rules_x = area.right().saturating_sub(RULES_SIDEBAR_WIDTH);
        let divider = Rect::new(rules_x.saturating_sub(1), area.y, 1, area.height);
        let left = Rect::new(
            area.x,
            area.y,
            divider.x.saturating_sub(area.x),
            area.height,
        );
        let rules = Rect::new(rules_x, area.y, RULES_SIDEBAR_WIDTH, area.height);
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

        self.transcript
            .sync_live_tail(left.width.max(1), active_key, |width| {
                chat_widget.active_cell_transcript_hyperlink_lines(width)
            });
        self.transcript.render_timeline(timeline, buf);
        chat_widget.render_rules_sidebar_bottom_pane(bottom, buf);
        Paragraph::new(
            (0..divider.height)
                .map(|_| Line::from("│".dim()))
                .collect::<Vec<_>>(),
        )
        .render(divider, buf);
        self.render_rules(rules, buf);
        chat_widget
            .rules_sidebar_cursor_pos(bottom)
            .map(|position| RulesSidebarCursor {
                position,
                // 光标形态取决于 composer 实际布局，不能拿整个 frame 的宽度重新计算。
                area: bottom,
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
                "Alt+Up/Down scroll  Ctrl+T close".dim()
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
