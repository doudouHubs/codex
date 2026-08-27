//! Full-screen overlay activation and ownership transitions.

use super::*;

impl App {
    pub(crate) fn activate_overlay(&mut self, tui: &mut tui::Tui, overlay: Overlay) {
        if self.main_transcript.is_some() {
            self.close_main_transcript_viewport(tui);
        }
        if self.rules_sidebar.take().is_some() {
            // Rules 侧栏与官方 overlay 共用 alternate screen，但不能同时拥有事件循环；
            // 切换时保留屏幕并恢复 pager 的滚轮策略，避免重进 alternate screen 覆盖原 viewport。
            self.rules_sidebar_generation = self.rules_sidebar_generation.wrapping_add(1);
            let _ = tui.disable_mouse_capture();
            tui.enable_alternate_scroll();
        } else {
            let _ = tui.enter_alt_screen();
        }
        self.overlay = Some(overlay);
        tui.frame_requester().schedule_frame();
    }
}
