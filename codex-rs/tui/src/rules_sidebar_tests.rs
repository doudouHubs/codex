use std::fs;

use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tempfile::tempdir;

use super::*;
use crate::keymap::RuntimeKeymap;

fn test_state() -> RulesSidebarState {
    RulesSidebarState::new(
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id"),
        PathBuf::from("/workspace"),
        Vec::new(),
        RuntimeKeymap::defaults().pager,
    )
}

fn render_panel(state: &mut RulesSidebarState, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    state.render_rules(area, &mut buffer);
    (0..height)
        .map(|y| {
            let row = (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

#[test]
fn parses_rule_system_list_contract() {
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
    let output = br#"{
        "session_id":"00000000-0000-0000-0000-000000000123",
        "rule_count":1,
        "rules":[{"title":"Testing","content":"Run focused tests."}]
    }"#;

    assert_eq!(
        parse_rules_output(output, thread_id),
        Ok(RulesSidebarLoad {
            rules: vec![RuleItem {
                title: "Testing".to_string(),
                content: "Run focused tests.".to_string(),
            }],
        })
    );
}

#[test]
fn rejects_rule_system_output_for_another_session() {
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
    let output = br#"{
        "session_id":"00000000-0000-0000-0000-000000000999",
        "rule_count":0,
        "rules":[]
    }"#;

    let error = parse_rules_output(output, thread_id).expect_err("session mismatch");
    assert_eq!(error, "rule-system returned rules for a different session.");
}

#[test]
fn locates_cli_from_enabled_rule_list_skill() {
    let root = tempdir().expect("tempdir");
    let skill = root.path().join("skills/rule-list/SKILL.md");
    let binary = root.path().join("bin").join(if cfg!(windows) {
        "rule-system.exe"
    } else {
        "rule-system"
    });
    fs::create_dir_all(skill.parent().expect("skill parent")).expect("skill directory");
    fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary directory");
    fs::write(&skill, "---\nname: rule-list\n---\n").expect("skill file");
    fs::write(&binary, []).expect("binary file");

    assert_eq!(locate_rule_system_binary(Some(&skill)), Ok(binary));
}

#[test]
fn rules_panel_loading_and_empty_snapshots() {
    let mut loading = test_state();
    loading.begin_load();
    insta::assert_snapshot!("rules_sidebar_loading", render_panel(&mut loading, 40, 10));

    let mut empty = test_state();
    insta::assert_snapshot!("rules_sidebar_empty", render_panel(&mut empty, 72, 10));
}

#[test]
fn rules_panel_wrap_error_and_scroll_snapshots() {
    let mut state = test_state();
    state.finish_load(Ok(RulesSidebarLoad {
        rules: vec![
            RuleItem {
                title: "Verification boundary".to_string(),
                content: "Run focused tests before claiming completion, and keep this deliberately long so the sidebar wrapping remains reviewable.".to_string(),
            },
            RuleItem {
                title: "Failure handling".to_string(),
                content: "Keep the last successful rules visible when refresh fails.".to_string(),
            },
        ],
    }));
    insta::assert_snapshot!("rules_sidebar_wrapped", render_panel(&mut state, 40, 12));

    // 分页边界取决于当前 viewport，先按目标高度渲染一次再翻页，避免沿用上一个宽松布局的 max_scroll。
    let _ = render_panel(&mut state, 40, 8);
    state.page_down();
    insta::assert_snapshot!("rules_sidebar_scrolled", render_panel(&mut state, 40, 8));

    state.finish_load(Err("rule-system failed: database busy".to_string()));
    insta::assert_snapshot!(
        "rules_sidebar_stale_error",
        render_panel(&mut state, 40, 12)
    );
}
