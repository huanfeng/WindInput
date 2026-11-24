//! 「英文状态隐藏工具栏」（`ui.toolbar.hide_in_english`）。

use crate::coordinator::Coordinator;
use wind_config::Config;
use wind_ui_types::UiCommand;

fn coord(
    hide_in_english: bool,
    chinese: bool,
) -> (
    std::sync::Arc<Coordinator>,
    std::sync::mpsc::Receiver<UiCommand>,
) {
    let mut cfg = Config::default();
    cfg.ui.toolbar.hide_in_english = hide_in_english;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    {
        let mut s = c.state.lock().unwrap_or_else(|e| e.into_inner());
        s.ime_active = true;
        s.has_edit_context = true;
        s.toolbar_visible = true;
        s.chinese_mode = chinese;
    }
    c.reset_toolbar_push_dedup();
    (c, rx)
}

fn last_toolbar(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Option<bool> {
    let mut last = None;
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            UiCommand::UpdateToolbar(_) => last = Some(true),
            UiCommand::HideToolbar => last = Some(false),
            _ => {}
        }
    }
    last
}

#[test]
fn english_mode_hides_toolbar_when_enabled() {
    let (c, rx) = coord(true, false);
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(false), "英文态应隐藏工具栏");
    assert!(
        !c.toolbar_wants_display(),
        "英文态下全屏复查闸应关：工具栏不显示，前台全不全屏无人关心"
    );
}

#[test]
fn switching_back_to_chinese_shows_toolbar_again() {
    let (c, rx) = coord(true, false);
    c.notify_toolbar();
    c.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .chinese_mode = true;
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(true), "切回中文应恢复工具栏");
    assert!(c.toolbar_wants_display());
}

#[test]
fn english_mode_keeps_toolbar_when_disabled() {
    let (c, rx) = coord(false, false);
    c.notify_toolbar();
    assert_eq!(last_toolbar(&rx), Some(true), "出厂关闭：英文态照常显示");
}
