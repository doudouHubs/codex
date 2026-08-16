use super::*;
use crossterm::event::KeyModifiers;

fn control_event(code: ModifierKeyCode, kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind(KeyCode::Modifier(code), KeyModifiers::NONE, kind)
}

#[test]
fn control_enables_capture_until_the_last_control_is_released() {
    let mut state = MouseCaptureState::default();
    assert!(!state.request_enable());

    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Press,
        )),
        Some(MouseCaptureAction::Enable)
    );
    assert!(state.request_enable());
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::RightControl,
            KeyEventKind::Press,
        )),
        Some(MouseCaptureAction::Ignore)
    );
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Release,
        )),
        Some(MouseCaptureAction::Ignore)
    );
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::RightControl,
            KeyEventKind::Release,
        )),
        Some(MouseCaptureAction::Disable)
    );
}

#[test]
fn repeated_and_duplicate_control_events_do_not_toggle_capture_twice() {
    let mut state = MouseCaptureState::default();
    assert!(!state.request_enable());

    let press = control_event(ModifierKeyCode::LeftControl, KeyEventKind::Press);
    assert_eq!(
        state.handle_control_key(&press),
        Some(MouseCaptureAction::Enable)
    );
    assert_eq!(
        state.handle_control_key(&press),
        Some(MouseCaptureAction::Ignore)
    );
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Repeat,
        )),
        Some(MouseCaptureAction::Ignore)
    );
    let release = control_event(ModifierKeyCode::LeftControl, KeyEventKind::Release);
    assert_eq!(
        state.handle_control_key(&release),
        Some(MouseCaptureAction::Disable)
    );
    assert_eq!(
        state.handle_control_key(&release),
        Some(MouseCaptureAction::Ignore)
    );
}

#[test]
fn surface_disable_while_control_is_held_prevents_capture_resume() {
    let mut state = MouseCaptureState::default();
    assert!(!state.request_enable());
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Press,
        )),
        Some(MouseCaptureAction::Enable)
    );

    state.request_disable();
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Release,
        )),
        Some(MouseCaptureAction::Ignore)
    );
}

#[test]
fn surface_enable_without_control_keeps_capture_disabled() {
    let mut state = MouseCaptureState::default();

    assert!(!state.request_enable());
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Release,
        )),
        Some(MouseCaptureAction::Ignore)
    );
}

#[test]
fn polled_control_state_enables_and_disables_capture() {
    let mut state = MouseCaptureState::default();
    assert!(!state.request_enable());

    assert_eq!(
        state.sync_control_key_mask(LEFT_CONTROL),
        MouseCaptureAction::Enable
    );
    assert_eq!(
        state.sync_control_key_mask(LEFT_CONTROL | RIGHT_CONTROL),
        MouseCaptureAction::Ignore
    );
    assert_eq!(
        state.sync_control_key_mask(RIGHT_CONTROL),
        MouseCaptureAction::Ignore
    );
    assert_eq!(state.sync_control_key_mask(0), MouseCaptureAction::Disable);
}

#[test]
fn ordinary_keys_are_not_control_capture_events() {
    let mut state = MouseCaptureState::default();
    let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);

    assert_eq!(state.handle_control_key(&key), None);
}
