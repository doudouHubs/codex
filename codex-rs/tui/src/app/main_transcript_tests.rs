use super::*;
use ratatui::layout::Rect;

#[test]
fn transcript_width_matches_the_embedded_scrollbar_reservation() {
    assert_eq!(transcript_width(Rect::new(0, 0, 80, 10)), 79);
    assert_eq!(transcript_width(Rect::new(0, 0, 1, 10)), 1);
    assert_eq!(transcript_width(Rect::new(0, 0, 0, 10)), 1);
}
