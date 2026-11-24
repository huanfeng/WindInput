use crate::coordinator::{Coordinator, ToolbarPush};
use wind_ui_types::ToolbarState;

fn state(label: &str) -> ToolbarPush {
    ToolbarPush::Shown(Box::new(ToolbarState {
        chinese_mode: true,
        icon_label: label.to_string(),
        caps_lock: false,
        full_width: false,
        chinese_punct: true,
        s2t_enabled: false,
        t2s_enabled: false,
        s2t_shown: false,
        soft_keyboard_on: false,
        input_blocked: false,
    }))
}

/// 去重的两个方向都要成立：挡住重复、放行变化。
///
/// 只测「挡住重复」会让一个恒返回 false 的实现也绿——那种缺陷的表现是工具栏
/// 彻底不更新，比重复推送严重得多。
#[test]
fn dedups_repeats_but_lets_changes_through() {
    let c = Coordinator::new_headless(wind_config::Config::default(), None);
    // 构造过程本身会推一次工具栏，缓存已非空——先归零，否则测的是构造顺序而非去重。
    c.reset_toolbar_push_dedup();

    assert!(c.take_toolbar_push_if_changed(state("五")), "首次必须下发");
    assert!(
        !c.take_toolbar_push_if_changed(state("五")),
        "内容相同应被挡下——焦点抖动时这条会被连推数次"
    );
    assert!(
        c.take_toolbar_push_if_changed(state("英")),
        "label 变了必须下发"
    );
}

/// `Hidden` 与 `Shown` 必须是两个可区分的值。
///
/// 若只缓存 `Option<ToolbarState>`、用 None 兼表「没推过」和「推过 Hide」，首帧的 Hide 会被
/// 误判成重复而跳过，工具栏就再也藏不掉。这里把两种状态的交替钉死。
#[test]
fn hidden_and_shown_are_distinguishable() {
    let c = Coordinator::new_headless(wind_config::Config::default(), None);
    c.reset_toolbar_push_dedup(); // 同上：构造已推过一次

    assert!(
        c.take_toolbar_push_if_changed(ToolbarPush::Hidden),
        "首帧 Hide 必须下发"
    );
    assert!(
        !c.take_toolbar_push_if_changed(ToolbarPush::Hidden),
        "重复 Hide 挡下"
    );
    assert!(
        c.take_toolbar_push_if_changed(state("五")),
        "Hide→Show 必须下发"
    );
    assert!(
        c.take_toolbar_push_if_changed(ToolbarPush::Hidden),
        "Show→Hide 必须下发，否则工具栏藏不掉"
    );
}

/// 配置热重载后必须重推：显隐策略（`ui.toolbar.visible`、全屏策略）不在 `ToolbarState` 里，
/// 光比内容会把该重推的那一次判成「没变」。
#[test]
fn reset_forces_next_push() {
    let c = Coordinator::new_headless(wind_config::Config::default(), None);
    c.reset_toolbar_push_dedup();
    assert!(c.take_toolbar_push_if_changed(state("五")));
    assert!(!c.take_toolbar_push_if_changed(state("五")));
    c.reset_toolbar_push_dedup();
    assert!(
        c.take_toolbar_push_if_changed(state("五")),
        "reset 之后同样的内容也必须下发一次"
    );
}
