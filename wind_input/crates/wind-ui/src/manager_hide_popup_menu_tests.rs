use super::*;
use crate::candidate_window::{CandidateWindow, CandidateWindowConfig};
use wind_ui_types::TooltipDoc;

fn doc() -> TooltipDoc {
    TooltipDoc {
        sections: vec![wind_ui_types::TooltipSection {
            title: None,
            inline: false,
            lines: vec![wind_ui_types::TooltipLine {
                text: "nǐ".into(),
                raw: 0,
            }],
        }],
    }
}

/// 候选窗 + 已显示、气泡菜单「开着」（抑制中）的气泡 + 一个菜单窗口。
fn setup() -> (CandidateWindow, Option<crate::popup_menu::PopupMenu>) {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx.clone()).unwrap();
    let t = w.tooltip_mut().expect("mock 气泡");
    t.show(&std::sync::Arc::new(doc()), 0, 0, 0, 10);
    t.set_menu_open(true);
    let m = crate::popup_menu::PopupMenu::new(tx).expect("mock 菜单");
    (w, Some(m))
}

fn tip_state(w: &mut CandidateWindow) -> (bool, bool) {
    let t = w.tooltip_mut().unwrap();
    (t.shown(), t.menu_suppressed())
}

/// 菜单已不可见（点菜单项时由它自己的 tick 收起）：协调器随后的 `HideMenu` 不得调到
/// `on_menu_dismissed`——否则气泡在「截图此窗口」处理之前就被藏掉。
#[test]
fn invisible_menu_does_not_dismiss_tooltip() {
    let (mut w, mut m) = setup();
    hide_popup_menu(&mut m, &mut w);
    assert_eq!(tip_state(&mut w), (true, true), "气泡与抑制都应原样保留");
}

/// 真收掉一个可见菜单（打字 / 失焦等路径）：按「菜单已关闭」处理，抑制不得残留。
#[test]
fn visible_menu_dismisses_tooltip() {
    let (mut w, mut m) = setup();
    m.as_mut().unwrap().show(
        vec![MenuItemSpec::leaf(
            "x",
            wind_ui_types::MenuKind::Copy,
            true,
            false,
        )],
        MenuAnchor::at_point(10, 10),
    );
    hide_popup_menu(&mut m, &mut w);
    // 非 Windows 下光标恒不在气泡上 → 隐藏。
    assert_eq!(tip_state(&mut w), (false, false));
}
