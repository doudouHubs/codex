use codex_app_server_protocol::SkillMetadata;
use codex_app_server_protocol::SkillScope;
use codex_app_server_protocol::SkillsListEntry;
use codex_app_server_protocol::SkillsListResponse;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;

use super::*;
use crate::app::test_support::make_test_app;
use crate::key_hint;

fn start_test_session(app: &mut App) {
    app.chat_widget.handle_thread_session(ThreadSessionState {
        thread_id: ThreadId::new(),
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: "gpt-test".to_string(),
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
        rollout_path: None,
    });
}

fn open_test_skill_popup(app: &mut App) {
    let skills = ["alpha", "beta"]
        .into_iter()
        .map(|name| SkillMetadata {
            name: name.to_string(),
            description: format!("{name} test skill"),
            short_description: None,
            interface: None,
            dependencies: None,
            path: test_path_buf(&format!("/tmp/{name}/SKILL.md")).abs(),
            scope: SkillScope::User,
            enabled: true,
        })
        .collect();
    app.chat_widget
        .set_skills_from_response(&SkillsListResponse {
            data: vec![SkillsListEntry {
                cwd: app.config.cwd.to_path_buf(),
                skills,
                errors: Vec::new(),
            }],
        });
    app.chat_widget.insert_str("$");
    assert!(!app.chat_widget.no_modal_or_popup_active());
}

#[tokio::test]
async fn rules_sidebar_toggle_preserves_transcript_and_composer() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.chat_widget.handle_paste("draft".to_string());
    let draft = app.chat_widget.composer_text_with_pending();
    let transcript_len = app.transcript_cells.len();

    app.open_rules_sidebar(&mut tui);
    assert!(app.rules_sidebar.is_some());
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    assert_eq!(app.transcript_cells.len(), transcript_len);

    app.handle_rules_sidebar_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
    );
    assert!(app.rules_sidebar.is_none());
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    assert_eq!(app.transcript_cells.len(), transcript_len);
}

#[tokio::test]
async fn non_sidebar_key_is_not_consumed() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);
    let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);

    assert!(!app.handle_rules_sidebar_key(&mut tui, key));
}

#[tokio::test]
async fn plain_arrows_follow_official_composer_history_while_sidebar_is_open() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.chat_widget.insert_str("first");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
    app.open_rules_sidebar(&mut tui);

    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    assert!(!app.handle_rules_sidebar_key(&mut tui, up));
    app.chat_widget.handle_key_event(up);
    assert_eq!(app.chat_widget.composer_text_with_pending(), "first");

    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    assert!(!app.handle_rules_sidebar_key(&mut tui, down));
    app.chat_widget.handle_key_event(down);
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
}

#[tokio::test]
async fn mouse_wheel_only_scrolls_the_left_timeline() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    tui.terminal.set_viewport_area(Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 120, /*height*/ 24,
    ));
    app.open_rules_sidebar(&mut tui);

    let left_timeline_scroll = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_rules_sidebar_mouse(&mut tui, left_timeline_scroll));

    let right_rules_scroll = MouseEvent {
        column: 119,
        ..left_timeline_scroll
    };
    assert!(!app.handle_rules_sidebar_mouse(&mut tui, right_rules_scroll));

    let composer_scroll = MouseEvent {
        column: 1,
        row: 23,
        ..left_timeline_scroll
    };
    assert!(!app.handle_rules_sidebar_mouse(&mut tui, composer_scroll));
}

#[tokio::test]
async fn official_chat_keys_are_not_consumed_by_sidebar_defaults() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);

    assert!(
        !app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),)
    );
    assert!(
        !app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),)
    );
    assert!(app.handle_rules_sidebar_key(
        &mut tui,
        KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL | KeyModifiers::ALT,),
    ));
}

#[tokio::test]
async fn official_transcript_overlay_replaces_rules_sidebar_state() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);

    // Backtrack 的第二次 Esc 也通过该官方入口打开 transcript；这里守住两种全屏状态互斥。
    app.open_transcript_overlay(&mut tui);

    assert!(app.rules_sidebar.is_none());
    assert!(matches!(app.overlay, Some(Overlay::Transcript(_))));
}

#[tokio::test]
async fn skill_popup_receives_arrows_while_rules_sidebar_is_open() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    open_test_skill_popup(&mut app);
    app.open_rules_sidebar(&mut tui);

    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    assert!(!app.handle_rules_sidebar_key(&mut tui, down));
    app.chat_widget.handle_key_event(down);
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.chat_widget.composer_text_with_pending(), "$beta ");
}

#[tokio::test]
async fn skill_popup_keeps_priority_over_view_shortcuts() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    open_test_skill_popup(&mut app);
    app.open_rules_sidebar(&mut tui);

    assert!(!app.handle_rules_sidebar_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
    ));
    assert!(!app.handle_rules_sidebar_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
    ));
    assert!(app.rules_sidebar.is_some());
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn modified_home_and_end_jump_transcript_without_stealing_composer_keys() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);

    assert!(app.handle_rules_sidebar_key(
        &mut tui,
        KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
    ));
    assert!(
        app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),)
    );
    assert!(
        !app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),)
    );
    assert!(
        !app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::End, KeyModifiers::NONE),)
    );
}

#[tokio::test]
async fn ctrl_e_transitions_from_rules_to_transcript_without_reentering_alt_screen() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);

    assert!(app.handle_rules_sidebar_key(
        &mut tui,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
    ));

    assert!(app.rules_sidebar.is_none());
    assert!(matches!(app.overlay, Some(Overlay::Transcript(_))));
}

#[tokio::test]
async fn stale_rule_load_is_ignored_after_close() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);
    let generation = app.rules_sidebar_generation;
    app.close_rules_sidebar(&mut tui);

    app.handle_rules_sidebar_loaded(
        &mut tui,
        generation,
        Ok(RulesSidebarLoad { rules: Vec::new() }),
    );

    assert!(app.rules_sidebar.is_none());
    assert_eq!(
        app.keymap.app.toggle_rules_sidebar,
        vec![key_hint::ctrl(KeyCode::Char('t'))]
    );
}
