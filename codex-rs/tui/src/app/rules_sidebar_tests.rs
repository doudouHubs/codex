use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
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
async fn alternate_scroll_arrows_are_consumed_by_transcript() {
    let mut app = make_test_app().await;
    start_test_session(&mut app);
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.set_alt_screen_enabled(false);
    app.open_rules_sidebar(&mut tui);

    assert!(
        app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),)
    );
    assert!(
        app.handle_rules_sidebar_key(&mut tui, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),)
    );
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
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
