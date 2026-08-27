use super::*;
use crossterm::event::KeyModifiers;

fn control_event(code: ModifierKeyCode, kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind(KeyCode::Modifier(code), KeyModifiers::NONE, kind)
}

#[test]
fn control_enables_capture_until_the_last_control_is_released() {
    let mut state = MouseCaptureState::default();
    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );

    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Press,
        )),
        Some(MouseCaptureAction::Enable)
    );
    assert!(state.capture_required());
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
fn shift_enables_capture_and_reports_its_held_state() {
    let mut state = MouseCaptureState::default();
    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );

    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftShift,
            KeyEventKind::Press,
        )),
        Some(MouseCaptureAction::Enable)
    );
    assert!(state.shift_is_pressed());
    assert!(state.capture_required());
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftShift,
            KeyEventKind::Release,
        )),
        Some(MouseCaptureAction::Disable)
    );
    assert!(!state.shift_is_pressed());
}

#[test]
fn repeated_and_duplicate_control_events_do_not_toggle_capture_twice() {
    let mut state = MouseCaptureState::default();
    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );

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
    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Press,
        )),
        Some(MouseCaptureAction::Enable)
    );

    assert_eq!(
        state.set_mode(MouseCaptureMode::Disabled),
        MouseCaptureAction::Disable
    );
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

    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Release,
        )),
        Some(MouseCaptureAction::Ignore)
    );
}

#[test]
fn always_capture_survives_control_key_transitions_and_restores_ctrl_held_mode() {
    let mut state = MouseCaptureState::default();

    assert_eq!(
        state.set_mode(MouseCaptureMode::Always),
        MouseCaptureAction::Enable
    );
    assert_eq!(
        state.handle_control_key(&control_event(
            ModifierKeyCode::LeftControl,
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
    assert!(state.capture_required());

    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Disable
    );
    assert!(!state.capture_required());
}

#[test]
fn polled_control_state_enables_and_disables_capture() {
    let mut state = MouseCaptureState::default();
    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );

    // Windows 不一定会把独立 Ctrl 事件送进 crossterm，轮询必须覆盖按下到释放的完整周期。
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
fn polled_shift_state_enables_and_disables_capture() {
    let mut state = MouseCaptureState::default();
    assert_eq!(
        state.set_mode(MouseCaptureMode::CtrlHeld),
        MouseCaptureAction::Ignore
    );

    assert_eq!(
        state.sync_control_key_mask(LEFT_SHIFT),
        MouseCaptureAction::Enable
    );
    assert!(state.shift_is_pressed());
    assert_eq!(state.sync_control_key_mask(0), MouseCaptureAction::Disable);
    assert!(!state.shift_is_pressed());
}

#[test]
fn ordinary_keys_are_not_control_capture_events() {
    let mut state = MouseCaptureState::default();
    let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);

    assert_eq!(state.handle_control_key(&key), None);
}
