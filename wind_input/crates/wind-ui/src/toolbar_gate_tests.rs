use super::*;

const MS: Duration = Duration::from_millis(1);

#[test]
fn first_update_defers_show() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    assert_eq!(g.on_update(t0, false), UpdateAction::Deferred);
    // 未到期不显示
    assert_eq!(g.tick_at(t0 + 119 * MS), GateTick::None);
    assert_eq!(g.tick_at(t0 + 120 * MS), GateTick::Show);
    assert!(!g.is_active());
}

#[test]
fn update_when_visible_renders_immediately() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    // 已可见：中英切换等内容更新不得被迟滞拖慢
    assert_eq!(g.on_update(t0, true), UpdateAction::RenderNow);
    assert!(!g.is_active());
}

/// 核心回归：QQ 密码框 DocMgr churn —— gained 后约 17ms 即 lost，工具栏不应出现。
///
/// ⚠ **必须模拟窗口期内的 tick**：真实事件循环每约 8ms 推进一次（无命令时 sleep 8ms），
/// 缺了这几次 tick，本用例在 `SHOW_DEBOUNCE = 0` 时**照样全绿**——锁住的就只是接口
/// 形状而非「迟滞必须长于 churn 间隔」这个关键性质。
#[test]
fn docmgr_churn_never_shows_toolbar() {
    let mut g = ToolbarGate::new();
    let mut t = Instant::now();
    for _ in 0..10 {
        assert_eq!(g.on_update(t, false), UpdateAction::Deferred);
        // 事件循环在 gained→lost 的 17ms 间隔内会 tick 两次，此时不得放行显示
        for elapsed in [8, 16] {
            assert_eq!(
                g.tick_at(t + elapsed * MS),
                GateTick::None,
                "churn 间隔内不得显示（SHOW_DEBOUNCE 必须显著大于 gained→lost 间隔）"
            );
        }
        // 17ms 后 focus_lost 抵达 → 撤销待显示，且不排隐藏（本就不可见）
        assert_eq!(g.on_hide(t + 17 * MS), HideAction::CancelledPending);
        assert!(!g.is_active(), "撤销后不应残留任何待定项");
        // 推进到远超两档迟滞：全程不得显示
        assert_eq!(g.tick_at(t + 300 * MS), GateTick::None);
        t += 180 * MS; // 下一轮 churn
    }
}

/// 对照组：正常点进输入框（无尾随 hide）→ 迟滞到期后正常显示。
#[test]
fn normal_focus_shows_after_debounce() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    g.on_update(t0, false);
    assert_eq!(g.tick_at(t0 + 200 * MS), GateTick::Show);
}

/// 对照组：Alt+Tab 应用间切换（hide 紧跟 update）→ 隐藏被取消，保持显示。
#[test]
fn alt_tab_hide_then_update_stays_visible() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    assert_eq!(g.on_hide(t0), HideAction::Scheduled);
    // 30ms 后新宿主 focus_gained 抵达（工具栏此刻仍可见）
    assert_eq!(g.on_update(t0 + 30 * MS, true), UpdateAction::RenderNow);
    assert!(!g.is_active(), "待定隐藏应已被取消");
    assert_eq!(g.tick_at(t0 + 500 * MS), GateTick::None);
}

/// 真正离开可编辑控件：隐藏照常在 50ms 后生效。
#[test]
fn hide_takes_effect_after_debounce() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    assert_eq!(g.on_hide(t0), HideAction::Scheduled);
    assert_eq!(g.tick_at(t0 + 49 * MS), GateTick::None);
    assert_eq!(g.tick_at(t0 + 50 * MS), GateTick::Hide);
    assert!(!g.is_active());
}

/// 连发多条 UpdateToolbar 不得把显示窗口一再推后（否则工具栏迟迟不出现）。
#[test]
fn repeated_updates_do_not_postpone_show() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    g.on_update(t0, false);
    g.on_update(t0 + 40 * MS, false);
    g.on_update(t0 + 80 * MS, false);
    // 仍以第一条为准：t0+120ms 到期
    assert_eq!(g.tick_at(t0 + 120 * MS), GateTick::Show);
}

/// 不变式：show_at 与 hide_at 不同时存在。
#[test]
fn show_and_hide_are_mutually_exclusive() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    g.on_update(t0, false); // 待显示
    assert_eq!(g.on_hide(t0 + 10 * MS), HideAction::CancelledPending);
    assert!(!g.is_active());

    g.on_hide(t0 + 20 * MS); // 待隐藏
    assert_eq!(g.on_update(t0 + 30 * MS, true), UpdateAction::RenderNow);
    assert!(!g.is_active());
}

/// 待显示期间工具栏不可见 → 此时到达的 update 仍走 Deferred，不改期。
#[test]
fn update_while_pending_keeps_deadline_and_stays_deferred() {
    let mut g = ToolbarGate::new();
    let t0 = Instant::now();
    assert_eq!(g.on_update(t0, false), UpdateAction::Deferred);
    assert_eq!(g.on_update(t0 + 60 * MS, false), UpdateAction::Deferred);
    assert_eq!(g.tick_at(t0 + 119 * MS), GateTick::None);
    assert_eq!(g.tick_at(t0 + 120 * MS), GateTick::Show);
}
