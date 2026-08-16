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

#[derive(Debug, Default)]
pub(super) struct MouseCaptureState {
    // surface 请求只决定当前界面是否允许捕获；物理捕获还必须满足 Ctrl 正在按住。
    requested: bool,
    control_keys_pressed: u8,
}

impl MouseCaptureState {
    pub(super) fn request_enable(&mut self) -> bool {
        self.requested = true;
        self.control_keys_pressed != 0
    }

    pub(super) fn request_disable(&mut self) {
        self.requested = false;
    }

    pub(super) fn is_requested(&self) -> bool {
        self.requested
    }

    pub(super) fn sync_control_key_mask(&mut self, control_keys_pressed: u8) -> MouseCaptureAction {
        let control_keys_pressed = control_keys_pressed & (LEFT_CONTROL | RIGHT_CONTROL);
        let was_pressed = self.control_keys_pressed != 0;
        let is_pressed = control_keys_pressed != 0;
        self.control_keys_pressed = control_keys_pressed;

        if !was_pressed && is_pressed && self.requested {
            // Windows 控制台不会把独立 Ctrl 事件交给 crossterm，轮询状态也必须复用同一状态机。
            MouseCaptureAction::Enable
        } else if was_pressed && !is_pressed && self.requested {
            MouseCaptureAction::Disable
        } else {
            MouseCaptureAction::Ignore
        }
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
                self.control_keys_pressed |= mask;
                if self.control_keys_pressed == mask && self.requested {
                    // 只有第一个 Ctrl 需要切换物理捕获，另一侧 Ctrl 只是延长按住周期。
                    Some(MouseCaptureAction::Enable)
                } else {
                    Some(MouseCaptureAction::Ignore)
                }
            }
            KeyEventKind::Repeat => Some(MouseCaptureAction::Ignore),
            KeyEventKind::Release => {
                if self.control_keys_pressed & mask == 0 {
                    return Some(MouseCaptureAction::Ignore);
                }
                self.control_keys_pressed &= !mask;
                if self.control_keys_pressed == 0 && self.requested {
                    // 松开最后一个 Ctrl 后必须立即交还终端，恢复原生划选和滚轮。
                    Some(MouseCaptureAction::Disable)
                } else {
                    Some(MouseCaptureAction::Ignore)
                }
            }
        }
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
