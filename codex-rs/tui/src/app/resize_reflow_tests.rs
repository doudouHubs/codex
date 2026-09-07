use super::*;
use crate::app::test_support::make_test_app;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::new_session_info;
use crate::legacy_core::config::TerminalResizeReflowMaxRows;
use crate::session_state::ThreadSessionState;
use codex_app_server_protocol::AskForApproval;
use codex_config::types::ApprovalsReviewer;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn plain_history_cells(count: usize) -> Vec<Arc<dyn HistoryCell>> {
    (0..count)
        .map(|index| {
            Arc::new(PlainHistoryCell::new(vec![Line::from(format!(
                "cell {index}"
            ))])) as Arc<dyn HistoryCell>
        })
        .collect()
}

fn rendered_line_text(line: &HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn user_history_cell(message: &str) -> Arc<dyn HistoryCell> {
    Arc::new(UserHistoryCell {
        message: message.to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    })
}

fn session_info_history_cell(app: &App) -> Arc<dyn HistoryCell> {
    let session = ThreadSessionState {
        thread_id: ThreadId::new(),
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: app.chat_widget.current_model().to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: app.config.cwd.clone(),
        runtime_workspace_roots: Vec::new(),
        instruction_source_paths: Vec::new(),
        reasoning_effort: None,
        collaboration_mode: None,
        personality: None,
        message_history: None,
        network_proxy: None,
        rollout_path: Some(PathBuf::new()),
    };
    Arc::new(new_session_info(
        &app.config,
        app.chat_widget.current_model(),
        &session,
        /*is_first_event*/ false,
        /*tooltip_override*/ None,
        /*auth_plan*/ None,
        /*show_fast_status*/ false,
    ))
}

#[tokio::test]
async fn latest_turn_reflow_starts_at_the_first_user_message_of_a_turn() {
    let mut app = make_test_app().await;

    app.transcript_cells.push(user_history_cell("first prompt"));
    app.mark_history_turn_start(Some("turn-1".to_string()));
    app.transcript_cells
        .push(Arc::new(PlainHistoryCell::new(vec![Line::from(
            "first answer",
        )])));
    app.transcript_cells.push(user_history_cell("steer prompt"));
    app.mark_history_turn_start(Some("turn-1".to_string()));
    app.transcript_cells
        .push(Arc::new(PlainHistoryCell::new(vec![Line::from(
            "final answer",
        )])));

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    let rendered_text = rendered
        .lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered_text.contains("first prompt"));
    assert!(rendered_text.contains("steer prompt"));
    assert!(rendered_text.contains("final answer"));
}

#[tokio::test]
async fn optimistic_user_message_is_the_latest_turn_projection_before_echo() {
    let mut app = make_test_app().await;

    app.transcript_cells.push(user_history_cell("old prompt"));
    app.mark_history_turn_start(Some("old-turn".to_string()));
    app.transcript_cells
        .push(Arc::new(PlainHistoryCell::new(vec![Line::from(
            "old answer",
        )])));
    app.transcript_cells
        .push(user_history_cell("optimistic prompt"));
    app.mark_history_turn_start(None);

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    let rendered_text = rendered
        .lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered_text.contains("optimistic prompt"));
    assert!(!rendered_text.contains("old prompt"));
    assert!(!rendered_text.contains("old answer"));
}

#[tokio::test]
async fn session_info_only_reflow_does_not_invent_an_earlier_messages_notice() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(32);
    app.transcript_cells = vec![session_info_history_cell(&app)];

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    let rendered_text = rendered
        .lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!rendered_text.contains("Earlier messages are available"));
}

#[tokio::test]
async fn removing_latest_turn_boundary_falls_back_to_remaining_user_turn() {
    let mut app = make_test_app().await;

    app.transcript_cells.push(user_history_cell("older prompt"));
    app.mark_history_turn_start(Some("older-turn".to_string()));
    app.transcript_cells
        .push(Arc::new(PlainHistoryCell::new(vec![Line::from(
            "older answer",
        )])));
    app.transcript_cells
        .push(user_history_cell("removed prompt"));
    app.mark_history_turn_start(Some("removed-turn".to_string()));
    app.transcript_cells.pop();

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    let rendered_text = rendered
        .lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered_text.contains("older prompt"));
    assert!(rendered_text.contains("older answer"));
    assert!(!rendered_text.contains("removed prompt"));
}

#[tokio::test]
async fn backtrack_rebuild_refreshes_the_rendered_history_tail() -> Result<()> {
    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.insert_history_cell(
        &mut tui,
        Box::new(UserHistoryCell {
            message: "remaining prompt".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }),
    );
    app.mark_history_turn_start(Some("remaining-turn".to_string()));
    app.insert_history_cell(
        &mut tui,
        Box::new(UserHistoryCell {
            message: "removed prompt".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }),
    );
    app.mark_history_turn_start(Some("removed-turn".to_string()));
    let expected_tail = app.transcript_cells[0].clone();
    app.transcript_cells.pop();

    let terminal_width = TerminalWidth::from(tui.terminal.last_known_screen_size);
    app.rebuild_transcript_after_backtrack(&mut tui, terminal_width)?;

    let rendered_tail = app
        .last_rendered_history_tail
        .as_ref()
        .and_then(|tail| tail.cell.upgrade())
        .expect("backtrack should refresh the rendered tail");
    assert!(Arc::ptr_eq(&rendered_tail, &expected_tail));
    Ok(())
}

#[tokio::test]
async fn resize_reflow_preserves_configured_scrollback_beyond_the_visible_viewport() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(32);
    app.transcript_cells = plain_history_cells(/*count*/ 64);
    let screen_size = Size::new(/*width*/ 80, /*height*/ 24);
    let chat_height = app.with_chat_widget_frame(screen_size.width, |height, _| height);
    let visible_history_rows = screen_size
        .height
        .saturating_sub(chat_height)
        .max(/*other*/ 1);

    app.update_visible_history_rows(screen_size);
    let rendered = app.render_transcript_lines_for_reflow(screen_size.width);

    assert_eq!(app.resize_reflow_max_rows(), Some(32));
    assert_eq!(rendered.lines.len(), 32);
    assert!(rendered.lines.len() > usize::from(visible_history_rows));
    assert_eq!(
        rendered.lines.last().map(rendered_line_text),
        Some("cell 63".to_string())
    );
}
#[tokio::test]
async fn initial_replay_renders_latest_turn_without_trimming_transcript() -> Result<()> {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(32);
    app.begin_initial_history_replay_buffer();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let boxed_user_history_cell = |message: &str| {
        Box::new(UserHistoryCell {
            message: message.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Box<dyn HistoryCell>
    };
    for cell in [
        boxed_user_history_cell("old prompt"),
        Box::new(PlainHistoryCell::new(vec![Line::from("old answer")])) as Box<dyn HistoryCell>,
        boxed_user_history_cell("latest prompt"),
        Box::new(PlainHistoryCell::new(vec![Line::from("latest answer")])) as Box<dyn HistoryCell>,
    ] {
        app.insert_history_cell(&mut tui, cell);
    }

    assert_eq!(app.transcript_cells.len(), 4);
    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    let rendered_text = rendered
        .lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered_text.contains("latest prompt"));
    assert!(rendered_text.contains("latest answer"));
    assert!(!rendered_text.contains("old prompt"));
    assert!(!rendered_text.contains("old answer"));
    assert_eq!(rendered.lines.len(), 6);

    app.finish_initial_history_replay_buffer(&mut tui);
    assert!(app.initial_history_replay_buffer.is_none());
    assert!(!tui.pending_history_lines_for_test().is_empty());
    insta::assert_snapshot!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n"),
        @r"
    Earlier messages are available — press ctrl + e to view the full transcript

    › latest prompt


    latest answer
    "
    );
    Ok(())
}

#[tokio::test]
async fn latest_turn_reflow_ignores_row_cap() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(2);
    app.transcript_cells = vec![
        user_history_cell("latest prompt"),
        Arc::new(AgentMarkdownCell::new(
            "line one\nline two\nline three".to_string(),
            std::path::Path::new("/tmp"),
        )),
    ];

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert!(rendered.lines.len() > 2);
    assert!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .any(|line| line.contains("line three"))
    );
    assert_eq!(app.transcript_cells.len(), 2);
}

#[tokio::test]
async fn resize_reflow_preserves_configured_scrollback_when_the_terminal_height_changes() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(48);
    app.transcript_cells = plain_history_cells(/*count*/ 64);

    app.update_visible_history_rows(Size::new(/*width*/ 80, /*height*/ 24));
    let smaller = app.render_transcript_lines_for_reflow(/*width*/ 80);
    app.update_visible_history_rows(Size::new(/*width*/ 80, /*height*/ 48));
    let larger = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(smaller.lines.len(), 48);
    assert_eq!(larger.lines.len(), 48);
    assert_eq!(
        smaller
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        larger
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        larger.lines.last().map(rendered_line_text),
        Some("cell 63".to_string())
    );
}

#[tokio::test]
async fn resize_reflow_preserves_explicitly_unlimited_history() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;
    app.transcript_cells = plain_history_cells(/*count*/ 20);

    app.update_visible_history_rows(Size::new(/*width*/ 80, /*height*/ 24));
    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(app.resize_reflow_max_rows(), None);
    assert_eq!(rendered.lines.len(), 39);
    assert_eq!(
        rendered.lines.first().map(rendered_line_text),
        Some("cell 0".to_string())
    );
    assert_eq!(
        rendered.lines.last().map(rendered_line_text),
        Some("cell 19".to_string())
    );
}

#[tokio::test]
async fn capped_resize_reflow_prepends_transcript_notice_without_changing_transcript() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(8);
    app.transcript_cells = plain_history_cells(/*count*/ 12);

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(rendered.lines.len(), 8);
    assert_eq!(app.transcript_cells.len(), 12);
    insta::assert_snapshot!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n"),
        @r"
    Earlier messages are available — press ctrl + e to view the full transcript
    cell 8

    cell 9

    cell 10

    cell 11
    "
    );
}

#[tokio::test]
async fn capped_resize_reflow_counts_wrapped_notice_rows() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(8);
    app.transcript_cells = plain_history_cells(/*count*/ 12);

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 28);

    assert_eq!(rendered.lines.len(), 8);
    insta::assert_snapshot!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n"),
        @r"
    Earlier messages are
    available — press ctrl + e
    to view the full transcript
    cell 9

    cell 10

    cell 11
    "
    );
}

#[tokio::test]
async fn one_row_history_cap_preserves_conversation_instead_of_notice() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(1);
    app.scrollback_has_older_history = true;
    app.transcript_cells = plain_history_cells(/*count*/ 2);

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(rendered.lines.len(), 1);
    insta::assert_snapshot!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n"),
        @r"cell 1"
    );
}

#[tokio::test]
async fn paginated_resize_reflow_prepends_transcript_notice_for_unloaded_history() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(32);
    app.scrollback_has_older_history = true;
    app.transcript_cells = plain_history_cells(/*count*/ 2);

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    insta::assert_snapshot!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n"),
        @r"
    Earlier messages are available — press ctrl + e to view the full transcript
    cell 0

    cell 1
    "
    );
}

#[tokio::test]
async fn scrollback_refill_uses_all_loaded_rows_not_latest_turn_projection() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(32);
    app.scrollback_has_older_history = true;
    app.transcript_cells = plain_history_cells(/*count*/ 20);
    app.transcript_cells
        .push(user_history_cell("latest prompt"));

    let latest_turn_rows = app
        .render_transcript_lines_for_reflow(/*width*/ 80)
        .lines
        .len();
    let loaded_rows = app.rendered_transcript_rows_for_scrollback(/*width*/ 80);

    assert!(latest_turn_rows < 32);
    assert!(loaded_rows >= 32);
    assert!(!app.scrollback_history_needs_top_up(loaded_rows));
}

#[tokio::test]
async fn scrollback_refill_still_triggers_for_underfilled_loaded_transcript() {
    let mut app = make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(32);
    app.scrollback_has_older_history = true;
    app.transcript_cells = plain_history_cells(/*count*/ 2);

    let loaded_rows = app.rendered_transcript_rows_for_scrollback(/*width*/ 80);

    assert!(loaded_rows < 32);
    assert!(app.scrollback_history_needs_top_up(loaded_rows));
}
