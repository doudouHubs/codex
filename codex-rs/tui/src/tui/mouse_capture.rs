use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::ModifierKeyCode;

const LEFT_CONTROL: u8 = 1;
const RIGHT_CONTROL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MouseCaptureAction {
    Ignore,
    Disable,
    Enable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum MouseCaptureMode {
    #[default]
    Disabled,
    CtrlHeld,
    Always,
}

#[derive(Debug, Default)]
pub(super) struct MouseCaptureState {
    // 普通聊天区只在 Ctrl 按住时捕获，侧栏等 alternate screen surface 则必须始终捕获滚轮。
    mode: MouseCaptureMode,
    control_keys_pressed: u8,
}

impl MouseCaptureState {
    pub(super) fn mode(&self) -> MouseCaptureMode {
        self.mode
    }

    pub(super) fn set_mode(&mut self, mode: MouseCaptureMode) -> MouseCaptureAction {
        let was_capturing = self.capture_required();
        self.mode = mode;
        capture_action(was_capturing, self.capture_required())
    }

    pub(super) fn capture_required(&self) -> bool {
        matches!(self.mode, MouseCaptureMode::Always)
            || (matches!(self.mode, MouseCaptureMode::CtrlHeld) && self.control_keys_pressed != 0)
    }

    pub(super) fn sync_control_key_mask(&mut self, control_keys_pressed: u8) -> MouseCaptureAction {
        let control_keys_pressed = control_keys_pressed & (LEFT_CONTROL | RIGHT_CONTROL);
        let was_capturing = self.capture_required();
        self.control_keys_pressed = control_keys_pressed;
        // Windows 控制台不会把独立 Ctrl 事件交给 crossterm，轮询必须覆盖按下到释放的完整周期。
        capture_action(was_capturing, self.capture_required())
    }

    pub(super) fn handle_control_key(
        &mut self,
        key_event: &KeyEvent,
    ) -> Option<MouseCaptureAction> {
        let mask = control_mask(key_event.code)?;
        match key_event.kind {
            KeyEventKind::Press => {
                if self.control_keys_pressed & mask != 0 {
                    return Some(MouseCaptureAction::Ignore);
                }
                let was_capturing = self.capture_required();
                self.control_keys_pressed |= mask;
                // 只有捕获需求发生变化时才切换物理模式；Always 模式下 Ctrl 只是普通输入修饰键。
                Some(capture_action(was_capturing, self.capture_required()))
            }
            KeyEventKind::Repeat => Some(MouseCaptureAction::Ignore),
            KeyEventKind::Release => {
                if self.control_keys_pressed & mask == 0 {
                    return Some(MouseCaptureAction::Ignore);
                }
                let was_capturing = self.capture_required();
                self.control_keys_pressed &= !mask;
                // Always 模式不能因为释放 Ctrl 交还鼠标；关闭侧栏后切回 CtrlHeld 才恢复原语义。
                Some(capture_action(was_capturing, self.capture_required()))
            }
        }
    }
}

fn capture_action(was_capturing: bool, should_capture: bool) -> MouseCaptureAction {
    match (was_capturing, should_capture) {
        (false, true) => MouseCaptureAction::Enable,
        (true, false) => MouseCaptureAction::Disable,
        _ => MouseCaptureAction::Ignore,
    }
}

fn control_mask(code: KeyCode) -> Option<u8> {
    match code {
        KeyCode::Modifier(ModifierKeyCode::LeftControl) => Some(LEFT_CONTROL),
        KeyCode::Modifier(ModifierKeyCode::RightControl) => Some(RIGHT_CONTROL),
        _ => None,
    }
}

#[cfg(test)]
#[path = "mouse_capture_tests.rs"]
mod tests;
