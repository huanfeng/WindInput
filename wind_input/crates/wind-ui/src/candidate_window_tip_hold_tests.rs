use super::*;
use crate::popup_menu::set_menu_visible;
use std::sync::mpsc::{Receiver, channel};

const DELAY: u64 = 60;

fn mouse(delay_ms: u64) -> (CandidateMouse, Receiver<UiEvent>) {
    set_menu_visible(false);
    let (tx, rx) = channel();
    let r = |x| Rect {
        x,
        y: 0.0,
        w: 10.0,
        h: 10.0,
    };
    let m = CandidateMouse {
        hit_rects: vec![(0, r(0.0)), (1, r(20.0))],
        skin_frame: None,
        skin_decor: Vec::new(),
        skin_click_rects: Vec::new(),
        decor_press: None,
        skin_click: None,
        pass_hwnd: None,
        events: tx,
        last_hover: -1,
        last_cursor: (i32::MIN, i32::MIN),
        engaged: false,
        engage_at: None,
        pending_raw: -1,
        engage_delay_ms: delay_ms,
        hwnd: HWND::default(),
        dragging: false,
        pressed: None,
        drag_anchor: (0, 0),
        drag_origin: (0, 0),
        drag_pin: None,
        drag_pin_margin: (0, 0),
        margin: (0, 0, 0, 0),
        double_click: crate::double_click::DoubleClick::new(),
        tip_hold: TipHold::Off,
        tip_released: false,
        tip_for: Rc::new(Cell::new(-1)),
        deferred: None,
        tip_rect: None,
        tip_visible: None,
        tip_best: None,
        last_pos: None,
    };
    (m, rx)
}

fn lp(x: i32, y: i32) -> LPARAM {
    LPARAM(((y << 16) | x) as isize)
}

fn real_move(m: &mut CandidateMouse, x: i32) {
    m.last_cursor = (i32::MIN, i32::MIN);
    m.on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), lp(x, 5));
}

fn right_click(m: &mut CandidateMouse, x: i32) {
    m.on_message(HWND::default(), WM_RBUTTONDOWN, WPARAM(0), lp(x, 5));
}

/// 远在任何延迟之后（但早于「等菜单」的兜底超时）。
fn later() -> Instant {
    Instant::now() + Duration::from_millis(1000)
}

fn hovers(rx: &Receiver<UiEvent>) -> Vec<i32> {
    rx.try_iter()
        .filter_map(|e| match e {
            UiEvent::Hover(i) => Some(i),
            _ => None,
        })
        .collect()
}

fn menu_requests(rx: &Receiver<UiEvent>) -> usize {
    rx.try_iter()
        .filter(|e| {
            matches!(
                e,
                UiEvent::RequestCandidateMenu { .. } | UiEvent::RequestMainMenu(_)
            )
        })
        .count()
}

fn selects(rx: &Receiver<UiEvent>) -> Vec<usize> {
    rx.try_iter()
        .filter_map(|e| match e {
            UiEvent::CandidateSelect(i) => Some(i),
            _ => None,
        })
        .collect()
}

/// 点选在松开时生效（同 macOS / Linux）：按下不选，松开仍在同一项才选，且只选一次。
#[test]
fn candidate_select_fires_on_release_over_same_item() {
    let (mut m, rx) = mouse(DELAY);
    m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
    assert!(selects(&rx).is_empty(), "按下就选了");
    m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(6, 5));
    assert_eq!(selects(&rx), vec![0]);
    m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(6, 5));
    assert!(selects(&rx).is_empty(), "一次按下只选一次");
}

/// 按下后移到别的候选 / 空白处松开 = 放弃。
#[test]
fn candidate_select_abandoned_when_released_elsewhere() {
    let (mut m, rx) = mouse(DELAY);
    m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
    m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(25, 5));
    m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
    m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(500, 5));
    assert!(selects(&rx).is_empty());
    assert!(!m.dragging, "按在候选上不起拖");
}

/// 按住期间候选窗收起（上屏 / Esc / 失焦）：之后的松开不选。
#[test]
fn candidate_select_dropped_when_hidden_while_pressed() {
    let (mut m, rx) = mouse(DELAY);
    m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
    m.reset_drag();
    m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(5, 5));
    assert!(selects(&rx).is_empty());
}

/// 已激活、悬停在候选 0 上（UI 画着的悬停也是 0）。
fn engaged_on_first(delay_ms: u64) -> (CandidateMouse, Receiver<UiEvent>) {
    let (mut m, rx) = mouse(delay_ms);
    real_move(&mut m, 5);
    m.flush(later(), -1);
    assert_eq!(hovers(&rx), vec![0]);
    (m, rx)
}

/// 右键时激活延迟还没到期：到期后不能再发 Hover——那正是「气泡在菜单弹出后冒出来、
/// 叠在菜单上」的来路。
#[test]
fn right_click_cancels_pending_engage() {
    let (mut m, rx) = mouse(DELAY);
    real_move(&mut m, 5);
    assert!(m.engage_at.is_some(), "首次真实移动武装闸门");
    right_click(&mut m, 5);
    m.flush(later(), -1);
    assert!(m.tip_hold.active());
    assert!(m.engage_at.is_none());
    assert!(hovers(&rx).is_empty(), "闸门已撤，不应再发悬停");
}

/// 审查探针（H1）：右键 → 菜单可见之前鼠标动了 → 菜单可见 → 到期。整段都不得发 Hover。
/// 右键到菜单可见之间「菜单开着」还是 false，只靠它挡闸门时这里会漏。
fn probe_right_move_menu_flush(delay_ms: u64) {
    let (mut m, rx) = engaged_on_first(delay_ms);
    right_click(&mut m, 5);
    real_move(&mut m, 25);
    m.flush(later(), 0);
    assert!(hovers(&rx).is_empty(), "等菜单阶段不得发悬停");
    set_menu_visible(true);
    m.flush(later(), 0);
    real_move(&mut m, 6);
    m.flush(later(), 0);
    assert_eq!(m.tip_hold, TipHold::InMenu);
    assert!(m.engage_at.is_none(), "菜单中不武装");
    assert!(hovers(&rx).is_empty(), "菜单中不得发悬停");
}

#[test]
fn move_before_menu_shows_does_not_rearm() {
    probe_right_move_menu_flush(DELAY);
}

/// `ui.tooltip.delay=0`：闸门一武装就到期，没有「等菜单」阶段时气泡会抢在菜单之前出来。
#[test]
fn move_before_menu_shows_does_not_rearm_with_zero_delay() {
    probe_right_move_menu_flush(0);
}

/// 菜单关闭后要重新移动、再走一次延迟才解除；UI 画着的悬停就是光标下的目标时由窗口
/// 本地补显示（不发 Hover，协调器不会因同值重绘）。
#[test]
fn release_after_fresh_engage_same_target() {
    let (mut m, rx) = engaged_on_first(DELAY);
    right_click(&mut m, 5);
    set_menu_visible(true);
    m.flush(Instant::now(), 0);
    set_menu_visible(false);
    m.flush(later(), 0);
    assert_eq!(m.tip_hold, TipHold::AwaitMove, "菜单一关、鼠标没动：仍压着");
    real_move(&mut m, 6);
    let armed = m.engage_at.expect("真实移动武装闸门");
    m.flush(armed - Duration::from_millis(1), 0);
    assert!(m.tip_hold.active(), "延迟没走完不解除");
    m.flush(armed, 0);
    assert_eq!(m.tip_hold, TipHold::Off);
    assert!(m.tip_released, "UI 画着的就是它，须本地补显示");
    assert!(hovers(&rx).is_empty());
}

/// 解除时目标变了：发 Hover，由协调器重绘带出气泡。
#[test]
fn release_after_fresh_engage_new_target() {
    let (mut m, rx) = engaged_on_first(DELAY);
    right_click(&mut m, 5);
    m.flush(later(), 0);
    real_move(&mut m, 25);
    // 没有菜单出现：等兜底超时后才算关闭。
    m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 2, 0);
    real_move(&mut m, 26);
    m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 3, 0);
    assert_eq!(m.tip_hold, TipHold::Off);
    assert!(!m.tip_released);
    assert_eq!(hovers(&rx), vec![1]);
}

/// M1：压制期间候选重绘过、协调器把悬停清成 -1（UI 画的是 -1），而 `last_hover` 还是 0。
/// 解除时回到同一候选，必须发 Hover(0)——按 `last_hover` 判会以为「没变」，气泡不再回来。
#[test]
fn release_after_repaint_resends_hover() {
    let (mut m, rx) = engaged_on_first(DELAY);
    right_click(&mut m, 5);
    set_menu_visible(true);
    m.flush(Instant::now(), 0);
    set_menu_visible(false);
    real_move(&mut m, 6);
    m.flush(later(), -1);
    assert_eq!(m.tip_hold, TipHold::Off);
    assert!(!m.tip_released);
    assert_eq!(hovers(&rx), vec![0]);
}

/// 兜底超时：请求被丢、菜单始终没出现，压制不能永久持续。
#[test]
fn await_menu_times_out_to_await_move() {
    let (mut m, _rx) = mouse(DELAY);
    right_click(&mut m, 5);
    let TipHold::AwaitMenu { until } = m.tip_hold else {
        panic!("右键后应在等菜单：{:?}", m.tip_hold);
    };
    m.flush(until - Duration::from_millis(1), -1);
    assert!(m.tip_hold.blocks_gate());
    m.flush(until, -1);
    assert_eq!(m.tip_hold, TipHold::AwaitMove);
}

/// 迟到的菜单（超时之后才出现）照样压住，已武装的闸门作废。
#[test]
fn late_menu_reenters_in_menu() {
    let (mut m, rx) = mouse(DELAY);
    right_click(&mut m, 5);
    m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 2, -1);
    real_move(&mut m, 6);
    assert!(m.engage_at.is_some());
    set_menu_visible(true);
    m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 3, -1);
    assert_eq!(m.tip_hold, TipHold::InMenu);
    assert!(m.engage_at.is_none());
    assert!(hovers(&rx).is_empty());
}

/// 菜单开着时候选窗自己收到的右键不理：这次按下由菜单轮询合成，两路都认会请求两遍。
#[test]
fn own_right_click_ignored_while_menu_visible() {
    let (mut m, rx) = mouse(DELAY);
    set_menu_visible(true);
    right_click(&mut m, 5);
    assert_eq!(menu_requests(&rx), 0);
    assert!(!m.tip_hold.active());
}

/// 「等菜单」阶段自己的右键不理：合成那一路已发出请求、菜单还没出现（轮询已把旧菜单
/// 收起，看不出「开着」），同一次右键的真实消息随后才到——不挡就请求两遍、菜单重弹。
/// 超时转入「等重新移动」后恢复。
#[test]
fn own_right_click_ignored_while_awaiting_menu() {
    let (mut m, rx) = mouse(DELAY);
    m.right_click(5.0, 5.0, (5, 5));
    assert_eq!(menu_requests(&rx), 1);
    right_click(&mut m, 5);
    assert_eq!(menu_requests(&rx), 0, "请求在途不得重发");
    m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 2, -1);
    right_click(&mut m, 5);
    assert_eq!(menu_requests(&rx), 1, "超时后恢复");
}

/// 组合结束 / 窗口重新出现：压制随悬停状态一并清掉，不带进下一轮。
#[test]
fn reset_hover_clears_hold() {
    let (mut m, _rx) = mouse(DELAY);
    right_click(&mut m, 5);
    m.reset_hover();
    assert_eq!(m.tip_hold, TipHold::Off);
    assert!(!m.tip_released);
}

// ---- 窗口层：气泡真的被收起 / 补回（M2），菜单外右键合成（L1） ----

fn doc() -> wind_ui_types::TooltipDoc {
    wind_ui_types::TooltipDoc {
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

/// 候选窗（mock）：两个带气泡的候选，UI 悬停在候选 0 上，气泡已显示。
fn window() -> (CandidateWindow, Receiver<UiEvent>) {
    set_menu_visible(false);
    let (tx, rx) = channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    let item = |t: &str| CandidateItem {
        text_styled: Default::default(),
        text: t.into(),
        code: String::new(),
        label: String::new(),
        tooltip: std::sync::Arc::new(doc()),
        comment: Default::default(),
        comment_above: Default::default(),
        no_index: false,
    };
    w.candidates = vec![item("你"), item("拟")];
    let r = |x| Rect {
        x,
        y: 0.0,
        w: 10.0,
        h: 10.0,
    };
    w.hit_rects = vec![(0, r(0.0)), (1, r(20.0))];
    w.mouse.borrow_mut().hit_rects = w.hit_rects.clone();
    w.hover = 0;
    w.visible = true;
    w.update_tooltip(0, 0);
    assert!(tip_shown(&w), "前提：气泡已显示");
    (w, rx)
}

fn tip_shown(w: &CandidateWindow) -> bool {
    w.tooltip.as_ref().is_some_and(|t| t.shown())
}

/// 右键当轮 tick 就收起已显示的气泡；压制期间重绘不再弹出；解除后补回。
#[test]
fn hold_hides_tip_and_repaint_keeps_it_hidden() {
    let (mut w, _rx) = window();
    w.mouse.borrow_mut().right_click(5.0, 5.0, (5, 5));
    w.tick();
    assert!(!tip_shown(&w), "右键后气泡须收起");
    w.update_tooltip(0, 0);
    assert!(!tip_shown(&w), "压制期间重绘不得弹出气泡");
    // 菜单出现又关闭，随后重新移动、走完延迟。
    set_menu_visible(true);
    w.tick();
    set_menu_visible(false);
    w.tick();
    {
        let mut m = w.mouse.borrow_mut();
        m.engage_delay_ms = 0;
        real_move(&mut m, 6);
    }
    w.tick();
    assert!(tip_shown(&w), "解除后同一候选的气泡须补回");
}

/// L1：菜单开着时在候选窗上右键（候选窗收不到，由菜单轮询转来）→ 关旧开新。
#[test]
fn outside_right_press_on_candidate_window_reopens_menu() {
    let (mut w, rx) = window();
    w.window_at = |_, _, _| true;
    w.menu_outside_press(25, 5, true);
    let evs: Vec<UiEvent> = rx.try_iter().collect();
    assert!(
        evs.iter()
            .any(|e| matches!(e, UiEvent::RequestCandidateMenu { page_local: 1, .. })),
        "命中候选 1 应请求其词条菜单：{evs:?}"
    );
    assert!(w.mouse.borrow().tip_hold.active(), "合成右键同样压住气泡");
    // 左键：只关菜单。
    w.menu_outside_press(25, 5, false);
    assert_eq!(menu_requests(&rx), 0);
}

/// 右键落在候选窗外（且不在气泡上）：不合成。
#[test]
fn outside_right_press_elsewhere_is_ignored() {
    let (mut w, rx) = window();
    w.window_at = |_, _, _| false;
    w.menu_outside_press(25, 5, true);
    assert_eq!(menu_requests(&rx), 0);
}

// ---- 悬停宽限与强制隐藏（设计 §7.6 C）----

#[test]
fn hover_move_table() {
    use HoverMove::*;
    // 气泡跟着候选 0：去往「无」宽限，去往候选 1 切换延迟。
    assert_eq!(hover_move(0, -1, 0), Defer(TIP_GRACE));
    assert_eq!(
        hover_move(0, TAG_PAGE_PREV, 0),
        Defer(TIP_GRACE),
        "翻页器按「无」"
    );
    assert_eq!(hover_move(0, 1, 0), Defer(TIP_SWITCH));
    // 气泡不跟着移开的那个候选（没显示 / 跟着别的）：即时。
    assert_eq!(hover_move(0, -1, -1), Now);
    assert_eq!(hover_move(0, 1, 1), Now);
    assert_eq!(hover_move(-1, 0, -1), Now);
    assert_eq!(
        hover_move(TAG_PAGE_PREV, 0, TAG_PAGE_PREV),
        Now,
        "翻页器没有气泡"
    );
    assert!(TIP_SWITCH < TIP_GRACE);
}

#[test]
fn resolve_deferred_table() {
    let t0 = Instant::now();
    let until = t0 + TIP_GRACE;
    assert_eq!(resolve_deferred(t0, until, false), Deferred::Wait);
    assert_eq!(resolve_deferred(until, until, false), Deferred::Fire);
    assert_eq!(
        resolve_deferred(t0, until, true),
        Deferred::Cancel,
        "进了气泡就撤"
    );
    assert_eq!(resolve_deferred(until, until, true), Deferred::Cancel);
}

/// 已激活、悬停在候选 0 上且气泡跟着它。
fn engaged_on_0() -> (CandidateMouse, Receiver<UiEvent>) {
    let (mut m, rx) = mouse(0);
    m.engaged = true;
    m.last_hover = 0;
    m.tip_for.set(0);
    (m, rx)
}

/// 候选 → 空隙 → 气泡：穿过空隙时不发 `Hover(-1)`；进了气泡则整个撤掉，悬停留在 0。
#[test]
fn gap_on_the_way_to_tip_keeps_hover() {
    let (mut m, rx) = engaged_on_0();
    real_move(&mut m, 15);
    assert!(hovers(&rx).is_empty(), "空隙里不得立刻清悬停");
    assert!(m.deferred.is_some());
    m.resolve_deferred(later(), true);
    assert!(hovers(&rx).is_empty(), "进了气泡：撤掉");
    assert!(m.deferred.is_none());
    assert_eq!(m.last_hover, 0);
}

/// 朝向判据：气泡在右侧顶端时，水平右移、斜着往上右移都算；往左、往下偏离得比靠近快、
/// 原地不动都不算。气泡在下方时（收起态横排）同理换一条轴。
#[test]
fn heading_to_tip_table() {
    let right_top = (100, 0, 200, 40);
    assert!(
        heading_to_tip((10, 120), (14, 120), right_top),
        "沿行水平右移"
    );
    assert!(heading_to_tip((10, 120), (14, 116), right_top), "斜着往上");
    assert!(
        !heading_to_tip((10, 120), (14, 126), right_top),
        "往下偏得比靠近快"
    );
    assert!(!heading_to_tip((10, 120), (6, 120), right_top), "往回走");
    assert!(!heading_to_tip((10, 120), (10, 120), right_top), "没动");
    assert!(
        heading_to_tip((10, 20), (12, 26), right_top),
        "跨度内上下随意"
    );
    let left = (-200, 0, -100, 40);
    assert!(heading_to_tip((10, 20), (6, 20), left), "气泡在左侧：左移");
    let below = (0, 50, 80, 120);
    assert!(
        heading_to_tip((40, 10), (40, 14), below),
        "气泡在下方：下移"
    );
    assert!(!heading_to_tip((40, 10), (40, 6), below));
    assert!(
        !heading_to_tip((150, 60), (151, 60), (100, 0, 200, 100)),
        "已在气泡上"
    );
}

/// 宽限里朝气泡继续移动：截止顺延；背离气泡：照旧取较早者、不顺延。
#[test]
fn moving_toward_tip_extends_the_grace() {
    let run = |tip: (i32, i32, i32, i32)| {
        let (mut m, _rx) = engaged_on_0();
        m.tip_rect = Some(tip);
        m.tip_visible = Some(Rc::new(Cell::new(true)));
        real_move(&mut m, 5);
        real_move(&mut m, 22);
        let first = m.deferred.expect("移开气泡所属候选应延后");
        std::thread::sleep(Duration::from_millis(2));
        real_move(&mut m, 25);
        (first, m.deferred.unwrap())
    };
    let (first, after) = run((100, 0, 200, 40));
    assert!(after > first, "朝右侧气泡移动：顺延");
    let (first, after) = run((-200, 0, -100, 40));
    assert_eq!(after, first, "背离气泡：不顺延");
}

/// 指针停在格子上左右抖动：一来一回刷不出离气泡的新低，截止不再往后推。
#[test]
fn jitter_does_not_keep_extending() {
    let (mut m, _rx) = engaged_on_0();
    m.tip_rect = Some((100, 0, 200, 40));
    m.tip_visible = Some(Rc::new(Cell::new(true)));
    real_move(&mut m, 5);
    real_move(&mut m, 25);
    real_move(&mut m, 26);
    let settled = m.deferred.unwrap();
    for x in [25, 26, 25, 26] {
        std::thread::sleep(Duration::from_millis(2));
        real_move(&mut m, x);
    }
    assert_eq!(m.deferred.unwrap(), settled, "抖动不顺延");
}

/// 气泡已自己收起（`tip_rect` 还是旧值）：不按它判朝向。
#[test]
fn hidden_tip_does_not_extend() {
    let (mut m, _rx) = engaged_on_0();
    m.tip_rect = Some((100, 0, 200, 40));
    m.tip_visible = Some(Rc::new(Cell::new(false)));
    real_move(&mut m, 5);
    real_move(&mut m, 22);
    let first = m.deferred.unwrap();
    std::thread::sleep(Duration::from_millis(2));
    real_move(&mut m, 25);
    assert_eq!(m.deferred.unwrap(), first);
}

/// 宽限到期仍不在气泡上（也没回来）：照发。
#[test]
fn gap_fires_after_grace() {
    let (mut m, rx) = engaged_on_0();
    real_move(&mut m, 15);
    m.resolve_deferred(Instant::now(), false);
    assert!(hovers(&rx).is_empty(), "未到期不发");
    m.resolve_deferred(later(), false);
    assert_eq!(hovers(&rx), vec![-1]);
    assert_eq!(m.last_hover, -1);
}

/// 宽限期内回到原候选：待发作废，到期也不发。
#[test]
fn return_to_same_candidate_cancels() {
    let (mut m, rx) = engaged_on_0();
    real_move(&mut m, 15);
    real_move(&mut m, 5);
    assert!(m.deferred.is_none());
    m.resolve_deferred(later(), false);
    assert!(hovers(&rx).is_empty());
}

/// 擦过相邻候选：不立刻切换，切换延迟到期才发；期间进了气泡则不切。
#[test]
fn crossing_neighbour_defers_switch() {
    let (mut m, rx) = engaged_on_0();
    real_move(&mut m, 25);
    assert!(hovers(&rx).is_empty(), "擦过候选 1 不立刻切");
    let until = m.deferred.expect("延后");
    assert!(until <= Instant::now() + TIP_SWITCH);
    m.resolve_deferred(until, false);
    assert_eq!(hovers(&rx), vec![1]);
    // 进了气泡的那种。
    let (mut m, rx) = engaged_on_0();
    real_move(&mut m, 25);
    m.resolve_deferred(later(), true);
    assert!(hovers(&rx).is_empty());
}

/// 先进空隙（宽限）、再到另一个候选：按较早的切换延迟收紧，不顺延。
#[test]
fn switch_after_gap_tightens_deadline() {
    let (mut m, _rx) = engaged_on_0();
    real_move(&mut m, 15);
    let grace = m.deferred.unwrap();
    real_move(&mut m, 25);
    assert!(m.deferred.unwrap() < grace);
}

/// 气泡没跟着移开的候选：悬停照旧即时。
#[test]
fn no_tip_no_grace() {
    let (mut m, rx) = engaged_on_0();
    m.tip_for.set(-1);
    real_move(&mut m, 15);
    assert_eq!(hovers(&rx), vec![-1]);
    assert!(m.deferred.is_none());
}

/// 窗口层：tick 在光标进了气泡时撤掉待发；到期时间进入 next_deadline。
#[test]
fn window_tick_cancels_when_cursor_on_tip() {
    let (mut w, rx) = window();
    assert_eq!(w.tip_for.get(), 0, "显示气泡即记下它跟着谁");
    {
        let mut m = w.mouse.borrow_mut();
        m.engaged = true;
        m.last_hover = 0;
        real_move(&mut m, 15);
    }
    let _ = hovers(&rx);
    let until = w.mouse.borrow().deferred.expect("延后");
    assert_eq!(w.next_deadline(), Some(until));
    w.tooltip_mut().unwrap().stub_on_tip(true);
    w.tick();
    assert!(w.mouse.borrow().deferred.is_none());
    assert!(hovers(&rx).is_empty());
}

/// Esc / 上屏 / 失焦（候选窗收起）：光标就停在气泡上、跟踪挂着，气泡也得收；待发的
/// 悬停与「气泡跟着谁」一并清掉。
#[test]
fn candidate_hide_force_hides_tip_under_cursor() {
    let (mut w, _rx) = window();
    {
        let t = w.tooltip_mut().unwrap();
        t.stub_on_tip(true);
        t.simulate_move();
    }
    w.mouse.borrow_mut().deferred = Some(later());
    w.hide();
    assert!(!tip_shown(&w), "候选窗收起，气泡不得残留");
    assert_eq!(w.tip_for.get(), -1);
    assert!(w.mouse.borrow().deferred.is_none());
}

/// 悬停变成「无」（协调器重绘）：`tip_for` 随之清掉，不再对后续移动延后。
#[test]
fn tip_for_clears_when_tip_goes_away() {
    let (mut w, _rx) = window();
    w.hover = -1;
    w.update_tooltip(0, 0);
    assert_eq!(w.tip_for.get(), -1);
}
