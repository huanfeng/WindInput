use super::*;
use std::sync::atomic::Ordering::Relaxed;
use wind_config::app_compat::{AppCompat, AppCompatRule};
use wind_ipc::protocol::caret_source::{GUI_CARET, TSF_SELECTION};

const PID: u32 = 4242;
const PROC: &str = "ai.exe";
const TOKEN: u64 = 0x4242_0000_0001;
const TOKEN_2: u64 = 0x5353_0000_0001;

/// 造协调器并保留 UI 通道：「气泡有没有发出去、怎么定位」只能从 `ui_tx` 上观察。
fn coord(
    position_mode: &str,
    fallback: &str,
) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
    let mut cfg = Config::default();
    cfg.ui.status.enabled = true;
    cfg.ui.status.display_mode = "temp".to_string();
    cfg.ui.status.show_on_focus = true;
    cfg.ui.status.position_mode = position_mode.to_string();
    cfg.ui.status.fallback_position = fallback.to_string();
    cfg.ui.status.offset_x = 3;
    cfg.ui.status.offset_y = 4;
    cfg.ui.status.custom_x = 500;
    cfg.ui.status.custom_y = 600;
    Coordinator::new_headless_with_ui(cfg, None)
}

/// 把焦点落到 `PROC` 上并装一条规则（`None` = 不装规则）。
fn focus_with_rule(c: &Coordinator, rule: Option<AppCompatRule>) {
    c.active_compat.lock().unwrap().pid = PID;
    c.pid_names.lock().unwrap().insert(PID, PROC.to_string());
    *c.app_compat.lock().unwrap() = AppCompat::from_rules(rule.into_iter().collect());
}

fn rule(
    mode: Option<StatusPositionMode>,
    xy: (i32, i32),
    fallback: Option<StatusFallback>,
) -> AppCompatRule {
    AppCompatRule {
        process: PROC.to_string(),
        status_position_mode: mode,
        status_x: xy.0,
        status_y: xy.1,
        status_fallback_position: fallback,
        ..Default::default()
    }
}

fn set_caret(c: &Coordinator, x: i32, y: i32, h: i32, source: i32) {
    let mut st = c.state.lock().unwrap();
    st.caret_x = x;
    st.caret_y = y;
    st.caret_height = h;
    st.caret_source = source;
}

/// 通道里所有 ShowStatusTip 的 `(x, y, caret_height, placement)`。
fn tips(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Vec<(i32, i32, i32, StatusTipPlacement)> {
    rx.try_iter()
        .filter_map(|c| match c {
            UiCommand::ShowStatusTip {
                x,
                y,
                caret_height,
                placement,
                ..
            } => Some((x, y, caret_height, placement)),
            _ => None,
        })
        .collect()
}

const CARET: StatusTipPlacement = StatusTipPlacement::Caret {
    offset_x: 3,
    offset_y: 4,
    above: false,
};

fn caret_data(x: i32, y: i32, height: i32, source: i32) -> CaretData {
    CaretData {
        x,
        y,
        height,
        composition_start_x: 0,
        composition_start_y: 0,
        source,
        composition_rect: None,
    }
}

// ── 纯判定：placement_for ──────────────────────────────────────────────────

#[test]
fn placement_table() {
    use StatusFallback as F;
    use StatusPositionMode as M;
    let pos = |mode, fallback| StatusPosition {
        mode,
        x: 7,
        y: 8,
        fallback,
    };
    let anchor = StatusTipPlacement::Anchor(StatusTipAnchor::ScreenTopRight);
    let a = StatusAnchor::ScreenTopRight;
    let p = |mode, fb, valid| Coordinator::placement_for(pos(mode, fb), valid, 3, 4, false);
    // 坐标可信：follow_caret 一律跟随光标，兜底不参与。
    assert_eq!(p(M::FollowCaret, F::Hide, true), Some(CARET));
    assert_eq!(p(M::FollowCaret, F::Anchor(a), true), Some(CARET));
    // 坐标不可信：按兜底。
    assert_eq!(p(M::FollowCaret, F::Last, false), Some(CARET));
    assert_eq!(p(M::FollowCaret, F::Hide, false), None);
    assert_eq!(p(M::FollowCaret, F::Anchor(a), false), Some(anchor));
    // fixed / 锚点不读光标，兜底不参与。
    for valid in [true, false] {
        assert_eq!(
            p(M::Fixed, F::Hide, valid),
            Some(StatusTipPlacement::Fixed { x: 7, y: 8 })
        );
        assert_eq!(p(M::Anchor(a), F::Hide, valid), Some(anchor));
    }
}

// ── show_tip：兜底 ─────────────────────────────────────────────────────────

/// 出厂兜底 `last` 下，无效坐标的路径与无此功能时一致：仍按跟随光标
/// 下发，坐标是 `resolve_caret_for_ui` 回退后的最近一次有效坐标；从未有过有效坐标时原样
/// 下发无效坐标（UI 侧临时显示）。
#[test]
fn last_fallback_keeps_previous_behavior_for_invalid_caret() {
    let (c, rx) = coord("follow_caret", "last");
    // 从未有过有效坐标：原样下发 (0,0)。
    set_caret(&c, 0, 0, 20, GUI_CARET);
    c.show_tip("中");
    assert_eq!(tips(&rx), vec![(0, 0, 20, CARET)]);
    // 有效坐标：照常跟随。
    set_caret(&c, 300, 400, 22, TSF_SELECTION);
    c.show_tip("中");
    assert_eq!(tips(&rx), vec![(300, 400, 22, CARET)]);
    // 又无效：回退到最近一次有效坐标。
    set_caret(&c, 0, 0, 0, GUI_CARET);
    c.show_tip("英");
    assert_eq!(tips(&rx), vec![(300, 400, 22, CARET)]);
}

/// 兜底 `hide`：坐标无效时不显示；坐标有效时照常显示（hide 不是「关掉气泡」）。
/// 判的是原始坐标：即便有最近一次有效坐标可回退，这次无效也不显示。
#[test]
fn hide_fallback_suppresses_only_invalid_caret() {
    let (c, rx) = coord("follow_caret", "hide");
    set_caret(&c, 300, 400, 22, TSF_SELECTION);
    c.show_tip("中");
    assert_eq!(tips(&rx).len(), 1, "有效坐标照常显示");
    set_caret(&c, 0, 0, 20, GUI_CARET);
    c.show_tip("英");
    assert!(tips(&rx).is_empty(), "无效坐标 + hide ⇒ 不显示");
    assert!(
        c.last_status_text.lock().unwrap().as_str() == "中",
        "没显示出去的文本不得记进去重缓存，否则下次同文本会被吞掉"
    );
}

/// 兜底为锚点：无效坐标下发 Anchor 定位；有效坐标仍跟随光标。
#[test]
fn anchor_fallback_sends_anchor_for_invalid_caret() {
    let (c, rx) = coord("follow_caret", "window_bottom_left");
    set_caret(&c, 0, 0, 20, GUI_CARET);
    c.show_tip("中");
    let t = tips(&rx);
    assert_eq!(t.len(), 1);
    assert_eq!(
        t[0].3,
        StatusTipPlacement::Anchor(StatusTipAnchor::WindowBottomLeft)
    );
    set_caret(&c, 300, 400, 22, TSF_SELECTION);
    c.show_tip("中");
    assert_eq!(tips(&rx), vec![(300, 400, 22, CARET)]);
}

/// 锚点模式：不看坐标，一律下发锚点。
#[test]
fn anchor_mode_ignores_caret() {
    let (c, rx) = coord("screen_center", "hide");
    for (x, y, h) in [(300, 400, 22), (0, 0, 0)] {
        set_caret(&c, x, y, h, GUI_CARET);
        c.show_tip("中");
        let t = tips(&rx);
        assert_eq!(t.len(), 1, "锚点模式不受兜底 hide 影响");
        assert_eq!(
            t[0].3,
            StatusTipPlacement::Anchor(StatusTipAnchor::ScreenCenter)
        );
    }
}

// ── 按应用覆盖 ─────────────────────────────────────────────────────────────

/// 规则压过全局；无规则 / 规则没配这一项时回落全局。
#[test]
fn rule_beats_global() {
    let (c, rx) = coord("fixed", "last");
    set_caret(&c, 300, 400, 22, TSF_SELECTION);
    focus_with_rule(&c, None);
    c.show_tip("中");
    assert_eq!(tips(&rx)[0].3, StatusTipPlacement::Fixed { x: 500, y: 600 });

    let a = StatusAnchor::ScreenBottomRight;
    focus_with_rule(
        &c,
        Some(rule(Some(StatusPositionMode::Anchor(a)), (0, 0), None)),
    );
    c.show_tip("中");
    assert_eq!(
        tips(&rx)[0].3,
        StatusTipPlacement::Anchor(StatusTipAnchor::ScreenBottomRight)
    );

    // 规则只配了兜底：定位方式仍跟全局（fixed）。
    focus_with_rule(&c, Some(rule(None, (0, 0), Some(StatusFallback::Hide))));
    c.show_tip("中");
    assert_eq!(tips(&rx)[0].3, StatusTipPlacement::Fixed { x: 500, y: 600 });
}

/// 方式与坐标同层：规则 fixed 但还没摆过（0,0）时用规则的 (0,0) 哨兵，
/// 不是全局那份为别的应用摆的 (500,600)。
#[test]
fn rule_mode_and_coords_come_from_the_same_layer() {
    let (c, rx) = coord("fixed", "last");
    set_caret(&c, 300, 400, 22, TSF_SELECTION);
    focus_with_rule(
        &c,
        Some(rule(Some(StatusPositionMode::Fixed), (0, 0), None)),
    );
    c.show_tip("中");
    assert_eq!(tips(&rx)[0].3, StatusTipPlacement::Fixed { x: 0, y: 0 });
    focus_with_rule(
        &c,
        Some(rule(Some(StatusPositionMode::Fixed), (70, 80), None)),
    );
    c.show_tip("中");
    assert_eq!(tips(&rx)[0].3, StatusTipPlacement::Fixed { x: 70, y: 80 });

    // 规则 follow_caret 压过全局 fixed。
    focus_with_rule(
        &c,
        Some(rule(Some(StatusPositionMode::FollowCaret), (0, 0), None)),
    );
    c.show_tip("中");
    assert_eq!(tips(&rx)[0].3, CARET);
}

/// 兜底独立回落：规则只配方式时兜底取全局；规则的兜底压过全局。
#[test]
fn rule_fallback_beats_global_fallback() {
    let (c, rx) = coord("follow_caret", "last");
    set_caret(&c, 0, 0, 20, GUI_CARET);
    focus_with_rule(&c, Some(rule(None, (0, 0), Some(StatusFallback::Hide))));
    c.show_tip("中");
    assert!(tips(&rx).is_empty(), "规则兜底 hide 压过全局 last");

    focus_with_rule(
        &c,
        Some(rule(Some(StatusPositionMode::FollowCaret), (0, 0), None)),
    );
    c.show_tip("中");
    assert_eq!(tips(&rx).len(), 1, "规则没配兜底 ⇒ 全局 last ⇒ 照常显示");
}

// ── 焦点气泡：挂起与锚点超时 ─────────────────────────────────────────────────

fn park_gen(c: &Coordinator) -> u64 {
    c.pending_focus_tip_gen.load(Relaxed)
}

/// 兜底为锚点：非 TSF 坐标挂起，到期仍无 TSF 坐标则显示在锚点，挂起位清掉。
#[test]
fn focus_tip_times_out_to_anchor() {
    let (c, rx) = coord("follow_caret", "screen_top_left");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    assert!(tips(&rx).is_empty(), "非 TSF 域坐标先挂起");
    assert!(c.pending_focus_tip.load(Relaxed));

    c.fire_focus_tip_timeout(park_gen(&c));
    let t = tips(&rx);
    assert_eq!(t.len(), 1, "到期应显示");
    assert_eq!(
        t[0].3,
        StatusTipPlacement::Anchor(StatusTipAnchor::ScreenTopLeft)
    );
    assert!(!c.pending_focus_tip.load(Relaxed), "显示后挂起位必须清掉");

    // 之后到达的 TSF 坐标不再补显示一次。
    c.handle_caret_update(&caret_data(473, 217, 28, TSF_SELECTION));
    assert!(tips(&rx).is_empty());
}

/// 真定时器接线：不手动触发，等共享定时器线程到期回调。
#[test]
fn focus_tip_timer_really_fires() {
    let (c, rx) = coord("follow_caret", "screen_center");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut got = Vec::new();
    while got.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        got = tips(&rx);
    }
    assert_eq!(
        got.first().map(|t| t.3),
        Some(StatusTipPlacement::Anchor(StatusTipAnchor::ScreenCenter)),
        "{FOCUS_TIP_ANCHOR_TIMEOUT_MS}ms 超时应由定时器线程触发"
    );
}

/// TSF 坐标先到：走光标，之后的超时不再显示。
#[test]
fn focus_tip_tsf_caret_beats_timeout() {
    let (c, rx) = coord("follow_caret", "screen_center");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    let token = park_gen(&c);
    c.handle_caret_update(&caret_data(473, 217, 28, TSF_SELECTION));
    let t = tips(&rx);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].3, CARET, "TSF 坐标先到 ⇒ 跟随光标");
    c.fire_focus_tip_timeout(token);
    assert!(tips(&rx).is_empty(), "已显示过，超时不得再弹一次");
}

/// 失焦作废：超时到期也不显示。
#[test]
fn focus_tip_timeout_cancelled_by_focus_lost() {
    let (c, rx) = coord("follow_caret", "screen_center");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    let token = park_gen(&c);
    c.hide_tip();
    c.fire_focus_tip_timeout(token);
    assert!(tips(&rx).is_empty(), "失焦之后的超时不得弹出气泡");
}

/// 下次焦点事件作废：旧挂起的超时（旧代际）不显示，只有新挂起的算数。
#[test]
fn focus_tip_timeout_superseded_by_next_focus() {
    let (c, rx) = coord("follow_caret", "screen_center");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    let old = park_gen(&c);
    c.show_focus_status_if_enabled(TOKEN_2);
    assert_ne!(park_gen(&c), old);
    c.fire_focus_tip_timeout(old);
    assert!(tips(&rx).is_empty(), "被取代的超时不得显示");
    c.fire_focus_tip_timeout(park_gen(&c));
    assert_eq!(tips(&rx).len(), 1);
}

/// 下次焦点落到 fixed / 锚点应用：直接显示并清掉旧挂起，旧超时不得再把气泡挪走。
#[test]
fn focus_tip_timeout_cancelled_when_next_focus_shows_directly() {
    let (c, rx) = coord("follow_caret", "screen_center");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    let old = park_gen(&c);
    focus_with_rule(
        &c,
        Some(rule(Some(StatusPositionMode::Fixed), (70, 80), None)),
    );
    c.show_focus_status_if_enabled(TOKEN_2);
    assert_eq!(tips(&rx).len(), 1, "fixed 应用直接显示");
    c.fire_focus_tip_timeout(old);
    assert!(tips(&rx).is_empty(), "旧挂起的超时不得再弹");
    // 旧挂起也不得被随后到达的 TSF 坐标消费、再弹一次。
    c.handle_caret_update(&caret_data(473, 217, 28, TSF_SELECTION));
    assert!(tips(&rx).is_empty(), "直接显示过就不再有挂起可补");
}

/// 兜底 `last` / `hide`：维持现状——挂起、不超时（等到期也不显示）。
#[test]
fn focus_tip_without_anchor_fallback_never_times_out() {
    for fb in ["last", "hide"] {
        let (c, rx) = coord("follow_caret", fb);
        set_caret(&c, 0, 1388, 20, GUI_CARET);
        c.show_focus_status_if_enabled(TOKEN);
        assert!(c.pending_focus_tip.load(Relaxed), "{fb}: 应挂起");
        c.fire_focus_tip_timeout(park_gen(&c));
        std::thread::sleep(std::time::Duration::from_millis(
            FOCUS_TIP_ANCHOR_TIMEOUT_MS * 2,
        ));
        assert!(tips(&rx).is_empty(), "{fb}: 不超时，不显示");
        assert!(
            c.pending_focus_tip.load(Relaxed),
            "{fb}: 挂起保持到 TSF 坐标或失焦"
        );
    }
}

/// 锚点模式（不是兜底）：不读 caret，焦点气泡直接显示，不挂起。
#[test]
fn focus_tip_anchor_mode_shows_immediately() {
    let (c, rx) = coord("window_center", "last");
    set_caret(&c, 0, 1388, 20, GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN);
    let t = tips(&rx);
    assert_eq!(t.len(), 1);
    assert_eq!(
        t[0].3,
        StatusTipPlacement::Anchor(StatusTipAnchor::WindowCenter)
    );
    assert!(!c.pending_focus_tip.load(Relaxed));
}
