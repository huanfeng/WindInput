use super::*;

const SEC: Duration = Duration::from_secs(1);

/// 启用并显示于 t0，返回 (状态机, t0)。
fn armed() -> (AutoHide, Instant) {
    let mut ah = AutoHide::new();
    ah.configure(true, 5000);
    let t0 = Instant::now();
    ah.on_shown(t0);
    (ah, t0)
}

#[test]
fn disabled_by_default_stays_inactive() {
    let mut ah = AutoHide::new();
    ah.on_shown(Instant::now()); // 未启用：不设 deadline
    assert!(!ah.is_active());
}

/// 光标移出后，计时应从移出那一刻起算满 delay。
///
/// 事件驱动下 tick 稀疏，不显式处理移出沿的话，离开时读到的是上一次唤醒时设的 deadline，
/// 多半已经过期，工具栏会在光标刚移开就开始淡出，而不是等满 5 秒。
#[test]
fn leaving_toolbar_restarts_timer_from_departure() {
    let (mut ah, t0) = armed();
    // 光标在栏内（稀疏 tick，模拟事件驱动：只在有消息/到期时才 tick）
    assert_eq!(ah.tick_at(t0 + SEC, true, false), AutoHideAction::None);
    // 6 秒后才轮到下一次 tick，此时光标已移出。上一次顺延设的 deadline 是 t0+6s，
    // 早已过期——没有移出沿处理的话这里会立刻 Fade(255)。
    assert_eq!(
        ah.tick_at(t0 + 7 * SEC, false, false),
        AutoHideAction::None,
        "光标刚移出就开始淡出：移出沿没有重置计时"
    );
    // 从移出时刻(t0+7s)起算 5 秒 = t0+12s 才该淡出；差一点点时仍不动。
    assert_eq!(
        ah.tick_at(t0 + 11 * SEC + Duration::from_millis(900), false, false),
        AutoHideAction::None,
        "未满 delay 就淡出"
    );
    assert_eq!(
        ah.tick_at(t0 + 12 * SEC, false, false),
        AutoHideAction::Fade(255),
        "满 delay 后未开始淡出"
    );
}

/// `next_deadline` 平时给出那个秒级到期时刻，供消息循环一觉睡到。
#[test]
fn next_deadline_reports_pending_timer() {
    let (ah, t0) = armed();
    assert_eq!(ah.next_deadline(t0), Some(t0 + 5 * SEC));

    // 未启用 / 已隐藏 → 无需唤醒。
    let mut idle = AutoHide::new();
    assert_eq!(idle.next_deadline(Instant::now()), None);
    idle.configure(true, 5000);
    idle.on_hidden();
    assert_eq!(idle.next_deadline(Instant::now()), None);
}

/// 淡出期间必须改要高频唤醒，否则 alpha 停在第一帧、工具栏「淡到一半卡住」。
#[test]
fn next_deadline_requests_frames_while_fading() {
    let (mut ah, t0) = armed();
    let fade_begin = t0 + 5 * SEC;
    assert_eq!(
        ah.tick_at(fade_begin, false, false),
        AutoHideAction::Fade(255)
    );
    assert_eq!(
        ah.next_deadline(fade_begin),
        Some(fade_begin + FADE_FRAME),
        "淡出中应按帧唤醒"
    );
    // 帧间隔必须远小于淡出总时长，否则淡出只有个位数帧。
    assert!(FADE_FRAME * 10 < FADE_DURATION);
}

#[test]
fn shown_arms_deadline_when_enabled() {
    let (ah, _) = armed();
    assert!(ah.is_active());
}

#[test]
fn no_action_before_deadline() {
    let (mut ah, t0) = armed();
    assert_eq!(ah.tick_at(t0 + 4 * SEC, false, false), AutoHideAction::None);
    assert!(ah.is_active());
}

#[test]
fn cursor_inside_extends_deadline() {
    let (mut ah, t0) = armed();
    // 已过原 deadline，但光标在内 → 顺延而非淡出
    assert_eq!(ah.tick_at(t0 + 6 * SEC, true, false), AutoHideAction::None);
    // 移出（t0+10s）→ 计时从这一刻起算，见 leaving_toolbar_restarts_timer_from_departure。
    assert_eq!(
        ah.tick_at(t0 + 10 * SEC, false, false),
        AutoHideAction::None
    );
    assert_eq!(
        ah.tick_at(t0 + 14 * SEC, false, false),
        AutoHideAction::None,
        "距移出未满 delay 就淡出"
    );
    assert_eq!(
        ah.tick_at(t0 + 15 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
}

#[test]
fn dragging_extends_deadline() {
    let (mut ah, t0) = armed();
    assert_eq!(ah.tick_at(t0 + 6 * SEC, false, true), AutoHideAction::None);
    assert!(ah.is_active());
}

#[test]
fn fades_linearly_then_hides() {
    let (mut ah, t0) = armed();
    // t0+5s 到期 → 淡出起点，首帧全亮
    assert_eq!(
        ah.tick_at(t0 + 5 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
    // 半程 ≈ 127
    match ah.tick_at(t0 + 5 * SEC + Duration::from_millis(500), false, false) {
        AutoHideAction::Fade(a) => assert!((120..=135).contains(&a), "alpha={a}"),
        other => panic!("expected Fade, got {other:?}"),
    }
    // 1 秒后 → 隐藏并清空
    assert_eq!(ah.tick_at(t0 + 6 * SEC, false, false), AutoHideAction::Hide);
    assert!(!ah.is_active());
}

#[test]
fn cursor_cancels_fade_and_rearms() {
    let (mut ah, t0) = armed();
    assert_eq!(
        ah.tick_at(t0 + 5 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
    // 淡出中移入 → Restore 且重新计时
    assert_eq!(
        ah.tick_at(t0 + 5 * SEC + Duration::from_millis(200), true, false),
        AutoHideAction::Restore
    );
    // 移出于 t0+9s，故淡出重新开始于 t0+14s（同上：按移出时刻起算，不是按最后一次
    // 「在内」的 tick 起算）。本测试的重点是上面那条 Restore，时刻只是佐证「重新计时了」。
    assert_eq!(ah.tick_at(t0 + 9 * SEC, false, false), AutoHideAction::None);
    assert_eq!(
        ah.tick_at(t0 + 13 * SEC, false, false),
        AutoHideAction::None
    );
    assert_eq!(
        ah.tick_at(t0 + 14 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
}

#[test]
fn hidden_clears_state() {
    let (mut ah, t0) = armed();
    ah.on_hidden();
    assert!(!ah.is_active());
    assert_eq!(ah.tick_at(t0 + 9 * SEC, false, false), AutoHideAction::None);
}

#[test]
fn disable_mid_fade_requests_restore() {
    let (mut ah, t0) = armed();
    assert_eq!(
        ah.tick_at(t0 + 5 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
    assert!(ah.configure(false, 5000)); // 淡出中关闭 → 需恢复不透明
    assert!(!ah.is_active());
}

#[test]
fn delay_ms_clamped_to_min_1s() {
    let mut ah = AutoHide::new();
    ah.configure(true, 0);
    let t0 = Instant::now();
    ah.on_shown(t0);
    // 0ms 被钳制为 1s：t0+0.5s 不应到期
    assert_eq!(
        ah.tick_at(t0 + Duration::from_millis(500), false, false),
        AutoHideAction::None
    );
}

/// 启用「悬停唤回」（假隐藏）并显示于 t0。
fn armed_ghost() -> (AutoHide, Instant) {
    let mut ah = AutoHide::new();
    ah.configure_hover_reveal(true);
    ah.configure(true, 5000);
    let t0 = Instant::now();
    ah.on_shown(t0);
    (ah, t0)
}

/// 假隐藏：淡出完毕不真隐藏，而是留一层几乎透明、仍可命中的窗口。
#[test]
fn hover_reveal_ghosts_instead_of_hiding() {
    let (mut ah, t0) = armed_ghost();
    assert_eq!(
        ah.tick_at(t0 + 5 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
    assert_eq!(
        ah.tick_at(t0 + 6 * SEC, false, false),
        AutoHideAction::Ghost,
        "淡出完毕应转入假隐藏，而不是 Hide"
    );
    // 假隐藏期间保持活动（光标移入要能被 tick 看见），但不要求定时唤醒：
    // 唤醒它的是 WM_MOUSEMOVE 本身。
    assert!(ah.is_active());
    assert_eq!(ah.next_deadline(t0 + 6 * SEC), None);
    // 光标没来：一直停在假隐藏，不重复提交。
    assert_eq!(
        ah.tick_at(t0 + 60 * SEC, false, false),
        AutoHideAction::None
    );
}

/// 光标移到原位 → 立即恢复不透明；移开后按原逻辑满 delay 再淡出。
#[test]
fn hovering_the_ghost_reveals_then_hides_again_after_leaving() {
    let (mut ah, t0) = armed_ghost();
    ah.tick_at(t0 + 5 * SEC, false, false);
    assert_eq!(
        ah.tick_at(t0 + 6 * SEC, false, false),
        AutoHideAction::Ghost
    );
    assert_eq!(
        ah.tick_at(t0 + 20 * SEC, true, false),
        AutoHideAction::Restore,
        "悬停在假隐藏的工具栏上应恢复显示"
    );
    assert_eq!(ah.tick_at(t0 + 30 * SEC, true, false), AutoHideAction::None);
    // t0+31s 移开 → t0+36s 才开始淡出。
    assert_eq!(
        ah.tick_at(t0 + 31 * SEC, false, false),
        AutoHideAction::None
    );
    assert_eq!(
        ah.tick_at(t0 + 35 * SEC, false, false),
        AutoHideAction::None
    );
    assert_eq!(
        ah.tick_at(t0 + 36 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
    assert_eq!(
        ah.tick_at(t0 + 37 * SEC, false, false),
        AutoHideAction::Ghost
    );
}

/// 状态刷新（切中英等，经 render → on_shown）把假隐藏的工具栏正常亮出来。
#[test]
fn shown_clears_ghost() {
    let (mut ah, t0) = armed_ghost();
    ah.tick_at(t0 + 5 * SEC, false, false);
    ah.tick_at(t0 + 6 * SEC, false, false);
    ah.on_shown(t0 + 10 * SEC);
    assert_eq!(ah.next_deadline(t0 + 10 * SEC), Some(t0 + 15 * SEC));
    assert_eq!(
        ah.tick_at(t0 + 14 * SEC, false, false),
        AutoHideAction::None
    );
}

/// 真隐藏（全屏 / 英文态 / 失焦经 HideToolbar）清掉假隐藏，不再响应悬停。
#[test]
fn hidden_clears_ghost() {
    let (mut ah, t0) = armed_ghost();
    ah.tick_at(t0 + 5 * SEC, false, false);
    ah.tick_at(t0 + 6 * SEC, false, false);
    ah.on_hidden();
    assert!(!ah.is_active());
    assert_eq!(ah.tick_at(t0 + 7 * SEC, true, false), AutoHideAction::None);
}

/// 假隐藏中关掉自动隐藏 → 要求恢复不透明（否则工具栏停在几乎透明的样子）。
#[test]
fn disabling_while_ghosted_requests_restore() {
    let (mut ah, t0) = armed_ghost();
    ah.tick_at(t0 + 5 * SEC, false, false);
    ah.tick_at(t0 + 6 * SEC, false, false);
    assert!(ah.configure(false, 5000));
    // 只关「悬停唤回」同理：此后该走真隐藏，假隐藏的窗口先亮回来重新计时。
    let (mut ah, t0) = armed_ghost();
    ah.tick_at(t0 + 5 * SEC, false, false);
    ah.tick_at(t0 + 6 * SEC, false, false);
    assert!(ah.configure_hover_reveal(false));
}

#[test]
fn reconfigure_enabled_mid_fade_keeps_fading_until_shown() {
    let (mut ah, t0) = armed();
    assert_eq!(
        ah.tick_at(t0 + 5 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
    // 淡出中以 enabled=true 改配置：configure 不打断淡出（返回 false）
    assert!(!ah.configure(true, 8000));
    assert!(ah.is_active());
    // 调用方（set_auto_hide）随后 on_shown：取消淡出、按新 delay 重新计时
    ah.on_shown(t0 + 5 * SEC + Duration::from_millis(300));
    assert_eq!(
        ah.tick_at(t0 + 12 * SEC, false, false),
        AutoHideAction::None
    );
    // 新 deadline = t0+5.3s+8s = t0+13.3s
    assert_eq!(
        ah.tick_at(t0 + 14 * SEC, false, false),
        AutoHideAction::Fade(255)
    );
}
