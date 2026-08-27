use super::*;
use crate::render::renderable::Renderable;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::layout::Rect;
use ratatui::text::Line;

#[tokio::test]
async fn mouse_click_uses_rendered_bottom_pane_area() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.bottom_pane
        .set_composer_text("abcd".to_string(), Vec::new(), Vec::new());
    chat.transcript.active_cell = Some(Box::new(PlainHistoryCell::new(vec![Line::from(
        "transcript",
    )])));

    let viewport = Rect::new(
        /*x*/ 0, /*y*/ 5, /*width*/ 60, /*height*/ 12,
    );
    let (cursor_x, cursor_y) = chat
        .as_renderable()
        .cursor_pos(viewport)
        .expect("composer cursor should be visible");
    assert!(cursor_y > viewport.y);

    chat.handle_mouse_event(
        viewport,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: cursor_x.saturating_sub(2),
            row: cursor_y,
            modifiers: KeyModifiers::CONTROL,
        },
    );

    assert_eq!(chat.bottom_pane.composer_cursor(), 2);
}
