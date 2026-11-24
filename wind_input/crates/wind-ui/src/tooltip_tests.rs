use super::*;
use wind_ui_types::{TooltipLine, TooltipSection};

fn doc() -> TooltipDoc {
    let line = |t: &str, raw| TooltipLine {
        text: t.into(),
        raw,
    };
    TooltipDoc {
        sections: vec![
            TooltipSection {
                title: Some("完整原文".into()),
                inline: false,
                // 一条原始行折成两条显示行
                lines: vec![line("一二三四五", 0), line("六七", 0)],
            },
            TooltipSection {
                title: Some("拼音".into()),
                inline: false,
                lines: vec![line("你：nǐ", 0), line("好：hǎo/hào", 1)],
            },
        ],
    }
}

fn tooltip() -> Tooltip {
    let (tx, _rx) = std::sync::mpsc::channel();
    Tooltip::new(tx).expect("创建气泡窗口（非 Windows 下是内存桩）")
}

/// 外观零回归的约束在「画的是同一串文本」：渲染路径仍是 `render_to_bgra(&str)` 画一个
/// 叶节点，只要 `TooltipDoc` 扁平化出的文本不变，像素就不变。
#[test]
fn doc_flattens_to_the_legacy_string() {
    let legacy = "[完整原文]\n一二三四五\n六七\n[拼音]\n你：nǐ\n好：hǎo/hào";
    assert_eq!(doc().to_plain_text(), legacy);
}

/// 命中换算「按行数均分」的前提：多行文本的高度 = 行数 × 单行高度，含 CJK 与 emoji 行
/// （渲染器行距钉成 UNIFORM，回退字体再高也不撑高行框）。只有真 DirectWrite 能验证；
/// mock 后端的高度本就是按行数算的。
#[cfg(windows)]
#[test]
fn multiline_height_is_lines_times_line_height() {
    let r = TextRenderer::new("Microsoft YaHei UI", FONT_PX).expect("DirectWrite");
    let one = r.measure_text("A").height;
    for text in [
        "A\n你好\n😀",
        "😀\nA",
        "你\n好\n吗\n👨\u{200D}👩\u{200D}👧",
        "[拼音]\n你：nǐ\n好：hǎo/hào",
    ] {
        let n = text.split('\n').count() as f32;
        let h = r.measure_text(text).height;
        assert!(
            (h - one * n).abs() < 0.5,
            "{text:?}: 高 {h}，应为 {n} × {one}"
        );
    }
}

/// 命中换算：标题行 / 内容行 / 折行后的第二条显示行 / 内边距。
///
/// 在非 Windows（mock 渲染器）下只验证换算的算术：矩形、按行均分、行号到 `(段, 原始行)`
/// 的映射。「每行真的等高」由上面的 `multiline_height_is_lines_times_line_height` 在
/// Windows 上兜底。
#[test]
fn hit_maps_client_point_to_section_and_raw_line() {
    let mut t = tooltip();
    let text = t.set_doc(&Arc::new(doc()), 3);
    let _ = t.render_to_bgra(&text);
    let h = t.hits.borrow();
    let b = h.text_box.expect("渲染后应记下文本块");
    assert_eq!(b.lines, 6);
    let line_h = b.h / 6.0;
    let at = |i: usize| {
        let y = (b.y + line_h * (i as f32 + 0.5)) as i32;
        let x = (b.x + b.w * 0.5) as i32;
        b.line_at(x, y).and_then(|l| h.doc.hit_at_line(l))
    };
    let hit = |section, raw_line| Some(TooltipHit { section, raw_line });
    assert_eq!(at(0), hit(0, None), "标题行");
    assert_eq!(at(1), hit(0, Some(0)));
    assert_eq!(at(2), hit(0, Some(0)), "折行的第二条显示行指回同一原始行");
    assert_eq!(at(3), hit(1, None));
    assert_eq!(at(5), hit(1, Some(1)));
    // 内边距：文本块上方、左侧都不算命中。
    assert_eq!(b.line_at(b.x as i32, (b.y - 1.0) as i32), None);
    assert_eq!(b.line_at((b.x - 1.0) as i32, (b.y + 1.0) as i32), None);
    assert_eq!(b.line_at(b.x as i32, (b.y + b.h + 1.0) as i32), None);
    assert_eq!(h.candidate, 3);
}

/// 菜单交互时序的判据表（设计 §7.6 B）。
#[test]
fn menu_step_table() {
    use MenuEvent::*;
    use MenuStep::*;
    let press = |right, own_menu, on_tip| OutsidePress {
        right,
        own_menu,
        on_tip,
    };
    // 首次右键：请求菜单；任何菜单开着时自己收到的右键不理（轮询那一路负责）。
    let own = |any_menu, in_flight| OwnRightDown {
        any_menu,
        in_flight,
    };
    assert_eq!(menu_step(own(false, false)), RequestMenu);
    assert_eq!(menu_step(own(true, false)), Ignore);
    assert_eq!(menu_step(own(false, true)), Ignore, "请求在途");
    // 气泡菜单开着、右键点在气泡另一处：重新请求菜单。
    assert_eq!(menu_step(press(true, true, true)), RequestMenu);
    // 左键点气泡：只关菜单，气泡去留由 Closed 决定。
    assert_eq!(menu_step(press(false, true, true)), Ignore);
    // 右键点在气泡外 / 关掉的是工具栏、状态等别人的菜单：不归气泡管。
    assert_eq!(menu_step(press(true, true, false)), Ignore);
    assert_eq!(menu_step(press(true, false, true)), Ignore);
    // 菜单关闭：光标在气泡上就留下，否则隐藏。
    assert_eq!(menu_step(Closed { on_tip: true }), Keep);
    assert_eq!(menu_step(Closed { on_tip: false }), Hide);
    // 离开：菜单开着时不理；光标仍在气泡上是伪离开；否则隐藏。
    let leave = |suppress, on_tip, spurious| Leave {
        suppress,
        on_tip,
        spurious,
    };
    assert_eq!(menu_step(leave(true, true, 0)), Ignore);
    assert_eq!(menu_step(leave(true, false, 0)), Ignore);
    assert_eq!(menu_step(leave(false, true, 0)), AwaitMove);
    assert_eq!(menu_step(leave(false, false, 0)), Grace, "真实离开先宽限");
    // 伪离开连续到上限：按真实离开处理，免得「补挂 → 伪离开」无限循环。
    assert_eq!(
        menu_step(leave(false, true, MAX_SPURIOUS_LEAVES - 1)),
        AwaitMove
    );
    assert_eq!(menu_step(leave(false, true, MAX_SPURIOUS_LEAVES)), Hide);
    // 复查定时器到期：光标仍在气泡上就补挂跟踪，否则隐藏。
    let recheck = |suppress, on_tip, on_anchor| Recheck {
        suppress,
        on_tip,
        on_anchor,
    };
    assert_eq!(menu_step(recheck(false, true, false)), Retrack);
    assert_eq!(menu_step(recheck(false, true, true)), Retrack);
    assert_eq!(menu_step(recheck(false, false, false)), Hide);
    // 宽限到期时光标回到了气泡所属的候选行：留下。
    assert_eq!(menu_step(recheck(false, false, true)), Idle);
    // 抑制中（光标没动就右键了气泡，菜单请求已发出）：复查不定去留。
    assert_eq!(menu_step(recheck(true, true, false)), Ignore);
    assert_eq!(menu_step(recheck(true, false, false)), Ignore);
}

fn tooltip_with_rx(on_tip: bool) -> (Tooltip, std::sync::mpsc::Receiver<UiEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Tooltip::new(tx).expect("mock 气泡");
    t.mouse.borrow_mut().window_at = if on_tip {
        |_, _, _| true
    } else {
        |_, _, _| false
    };
    (t, rx)
}

fn own_right_down(t: &Tooltip) {
    t.mouse
        .borrow_mut()
        .on_message(HWND::default(), WM_RBUTTONDOWN, WPARAM(0), LPARAM(0));
}

fn menu_requested(rx: &std::sync::mpsc::Receiver<UiEvent>) -> bool {
    rx.try_iter()
        .any(|e| matches!(e, UiEvent::RequestTooltipMenu { .. }))
}

/// 任何菜单可见时气泡自己的 `WM_RBUTTONDOWN` 不再请求菜单：若 Windows 真把这次按下投到
/// 气泡上，它与菜单轮询转来的那次是同一次右键，两路都认会请求两遍菜单。
#[test]
fn own_right_down_requests_menu_only_without_visible_menu() {
    let (t, rx) = tooltip_with_rx(true);
    crate::popup_menu::set_menu_visible(false);
    own_right_down(&t);
    assert!(menu_requested(&rx));
    assert!(t.suppress_hide.get(), "右键即抑制离开隐藏");
    // 协调器回应（请求不再在途），菜单随后可见。
    t.mouse.borrow().requested_at.set(None);
    crate::popup_menu::set_menu_visible(true);
    own_right_down(&t);
    crate::popup_menu::set_menu_visible(false);
    assert!(!menu_requested(&rx), "菜单开着时不再请求");
}

/// 请求在途时自己的右键不理：菜单轮询合成的那次已发出请求、菜单还没出现，同一次右键的
/// 真实消息随后才到。协调器回应（`SetTooltipMenuOpen(true)`）或超时后恢复。
#[test]
fn own_right_down_ignored_while_request_in_flight() {
    let (mut t, rx) = tooltip_with_rx(true);
    crate::popup_menu::set_menu_visible(false);
    t.set_menu_open(true);
    assert!(t.on_menu_outside_press(10, 10, true), "合成那一路发出请求");
    assert!(menu_requested(&rx));
    own_right_down(&t);
    assert!(!menu_requested(&rx), "同一次右键的真实消息不得再请求");
    // 协调器回应 → 在途清掉；菜单关闭后再右键照常。
    t.set_menu_open(true);
    t.set_menu_open(false);
    own_right_down(&t);
    assert!(menu_requested(&rx));
    // 请求被丢：超过回应时限后不再挡。
    let stale = std::time::Instant::now() - crate::popup_menu::MENU_REPLY_TIMEOUT * 2;
    t.mouse.borrow().requested_at.set(Some(stale));
    own_right_down(&t);
    assert!(menu_requested(&rx), "超时后恢复");
}

/// 气泡菜单开着时在气泡上再右键（气泡自己收不到，由菜单轮询转来）：按新位置重新请求。
#[test]
fn outside_right_press_on_tip_reopens_own_menu() {
    let (mut t, rx) = tooltip_with_rx(true);
    t.set_menu_open(true);
    assert!(t.on_menu_outside_press(10, 10, true));
    assert!(menu_requested(&rx));
    // 左键：只关菜单，不请求。
    assert!(!t.on_menu_outside_press(10, 10, false));
    assert!(!menu_requested(&rx));
}

/// 开着的是工具栏 / 状态等别人的菜单：右键气泡只关那个菜单，不弹气泡菜单。
#[test]
fn outside_right_press_with_foreign_menu_is_ignored() {
    let (mut t, rx) = tooltip_with_rx(true);
    assert!(!t.on_menu_outside_press(10, 10, true));
    assert!(!menu_requested(&rx));
}

/// 气泡菜单被打字 / 失焦等路径收掉（协调器只发 `HideMenu`）：`on_menu_dismissed` 收口
/// 抑制标志，此后气泡右键照常有效，别人的菜单开着时也不再被误认成气泡的菜单。
#[test]
fn dismissed_menu_clears_suppress_and_right_click_still_works() {
    let (mut t, rx) = tooltip_with_rx(true);
    t.visible.set(true);
    crate::popup_menu::set_menu_visible(false);
    own_right_down(&t);
    t.set_menu_open(true);
    let _ = menu_requested(&rx);
    t.on_menu_dismissed();
    assert!(!t.suppress_hide.get(), "抑制标志不得残留");
    assert!(t.mouse_over.get(), "光标在气泡上：留下并重挂跟踪");
    own_right_down(&t);
    assert!(menu_requested(&rx), "再右键仍然请求菜单");
    // 模拟随后换成工具栏菜单开着：标志若残留，这次右键会被当成重开气泡菜单。
    t.on_menu_dismissed();
    assert!(!t.on_menu_outside_press(10, 10, true));
}

/// 菜单关闭时光标不在气泡上：`mouse_over` 即便残留 true 也要隐藏，并解除抑制。
#[test]
fn menu_closed_off_tip_hides_even_with_stale_mouse_over() {
    let (mut t, _rx) = tooltip_with_rx(false);
    t.visible.set(true);
    t.set_menu_open(true);
    t.mouse_over.set(true);
    t.set_menu_open(false);
    assert!(!t.suppress_hide.get());
    assert!(!t.mouse_over.get());
    assert!(!t.shown());
}

/// 菜单关闭时光标在气泡上：留下。
#[test]
fn menu_closed_on_tip_keeps_it() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    assert!(t.shown() && t.mouse_over.get());
}

fn mouse_leave(t: &Tooltip) {
    t.mouse
        .borrow_mut()
        .on_message(HWND::default(), WM_MOUSELEAVE, WPARAM(0), LPARAM(0));
}

/// 菜单关闭、留下气泡并重挂跟踪后，系统当场投来的离开是伪的（光标仍在气泡上）：气泡
/// 留下，跟踪清掉等下一次移动重挂。左键点气泡关菜单时走的就是这条。
#[test]
fn spurious_leave_after_menu_close_keeps_tip() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    assert!(t.shown(), "伪离开不隐藏");
    assert!(t.mouse_over.get(), "光标仍在气泡上");
    assert!(!t.mouse.borrow().tracking, "跟踪失效，等下一次移动重挂");
    // 下一次真实移动重挂跟踪。
    t.mouse
        .borrow_mut()
        .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
    assert!(t.mouse.borrow().tracking);
}

/// 真离开（光标已不在气泡上）：先宽限、不当场藏；到期光标既不在气泡也不在候选行上则
/// 隐藏，且 `visible` 同步为 false——此后 `hide()` 按「已隐藏」幂等跳过，再显示时照常
/// 置回 true。
#[test]
fn real_leave_hides_and_syncs_visible() {
    let (mut t, _rx) = tooltip_with_rx(false);
    t.visible.set(true);
    t.mouse_over.set(true);
    mouse_leave(&t);
    assert!(t.shown(), "宽限期内不藏");
    assert!(t.mouse.borrow().recheck_pending, "离开即开宽限");
    recheck_fires(&t);
    assert!(!t.shown(), "离开隐藏须同步 visible");
    assert!(!t.mouse_over.get());
    assert!(
        matches!(_rx.try_recv(), Ok(UiEvent::Hover(-1))),
        "气泡自己收起时悬停清成「无」，移回原候选才会重新出气泡"
    );
    t.hide();
    assert!(!t.shown());
    t.show(&Arc::new(doc()), 0, 10, 10, 20);
    assert!(t.shown());
}

/// 菜单开着时的离开（移向菜单）：不隐藏，光标在不在气泡上都一样。
#[test]
fn leave_while_menu_open_is_ignored() {
    for on_tip in [false, true] {
        let (mut t, _rx) = tooltip_with_rx(on_tip);
        t.visible.set(true);
        t.set_menu_open(true);
        mouse_leave(&t);
        assert!(t.shown());
        assert!(!t.mouse_over.get());
    }
}

fn set_on_tip(t: &Tooltip, on_tip: bool) {
    t.mouse.borrow_mut().window_at = if on_tip {
        |_, _, _| true
    } else {
        |_, _, _| false
    };
}

fn recheck_fires(t: &Tooltip) {
    t.mouse.borrow_mut().on_message(
        HWND::default(),
        crate::sys::WM_TIMER,
        WPARAM(RECHECK_TIMER),
        LPARAM(0),
    );
}

/// 菜单关闭留下气泡、随即伪离开，而用户没在气泡上移动一下就把光标移走了：气泡收不到
/// `WM_MOUSEMOVE`，`mouse_over` 停在 true。此后候选窗收起 / 悬停换走调 `hide()` 必须
/// 问真实光标把它藏掉，不能按「鼠标在上面」一直推迟。
#[test]
fn hide_after_spurious_leave_checks_real_cursor() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    assert!(t.shown() && t.mouse_over.get(), "伪离开留下");
    // 光标仍在气泡上：照旧推迟。
    t.hide();
    assert!(t.shown(), "光标还在气泡上，推迟隐藏");
    set_on_tip(&t, false);
    t.hide();
    assert!(!t.shown(), "光标已离开，hide 不得被陈旧的 mouse_over 挡住");
    assert!(!t.mouse_over.get());
    assert!(!t.mouse.borrow().recheck_pending, "复查随隐藏撤掉");
}

/// 伪离开即启动复查；到期时光标仍在气泡上 → 补挂跟踪、气泡留下。
#[test]
fn recheck_on_tip_retracks() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    assert!(t.mouse.borrow().recheck_pending, "伪离开启动复查");
    recheck_fires(&t);
    let m = t.mouse.borrow();
    assert!(m.tracking && !m.recheck_pending);
    assert!(t.shown());
}

/// 复查到期时光标已不在气泡上 → 隐藏（这条路不依赖任何人再调 `hide()`）。
#[test]
fn recheck_off_tip_hides() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    set_on_tip(&t, false);
    recheck_fires(&t);
    assert!(!t.shown());
    assert!(!t.mouse_over.get());
}

/// 真实移动先到：复查撤掉，到期消息（若已在队列里）也不再动气泡。
#[test]
fn real_move_cancels_recheck() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    t.mouse
        .borrow_mut()
        .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
    assert!(!t.mouse.borrow().recheck_pending);
    set_on_tip(&t, false);
    recheck_fires(&t);
    assert!(t.shown(), "已撤掉的复查不得再隐藏");
}

/// 系统始终认定光标不在气泡上（补挂一次、伪离开一次）：到上限后按真实离开隐藏，不无限循环。
#[test]
fn spurious_leaves_are_capped() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    for i in 0..MAX_SPURIOUS_LEAVES {
        mouse_leave(&t);
        assert!(t.shown(), "第 {} 条伪离开仍留下", i + 1);
        recheck_fires(&t);
        assert!(t.mouse.borrow().tracking, "复查补挂");
    }
    mouse_leave(&t);
    assert!(!t.shown(), "超过上限按真实离开处理");
}

/// 气泡菜单再次打开（菜单开着时右键气泡另一处）：伪离开的复查撤掉——它到期时光标在菜单上。
#[test]
fn menu_reopen_cancels_recheck() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    t.set_menu_open(true);
    assert!(!t.mouse.borrow().recheck_pending);
    set_on_tip(&t, false);
    recheck_fires(&t);
    assert!(t.shown());
}

/// 伪离开等复查期间光标移到另一个候选，协调器重绘直接 `show` 新内容（不经 `hide`）：
/// 复查到期时光标在候选窗上，不得把刚显示的新气泡当「离开」藏掉。
#[test]
fn probe_show_after_spurious_leave_survives_recheck() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    set_on_tip(&t, false);
    t.show(&Arc::new(doc()), 1, 10, 10, 20);
    recheck_fires(&t);
    assert!(t.shown(), "新气泡不得被旧复查藏掉");
}

/// 内容原地刷新（光标仍在气泡上）：复查照旧在走。
#[test]
fn show_on_tip_keeps_recheck() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    t.show(&Arc::new(doc()), 0, 10, 10, 20);
    assert!(t.mouse.borrow().recheck_pending);
}

/// 光标没动就右键气泡：请求发出、抑制已开，协调器回应晚于复查时复查不得把气泡藏掉。
#[test]
fn recheck_during_menu_request_is_ignored() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    crate::popup_menu::set_menu_visible(false);
    own_right_down(&t);
    set_on_tip(&t, false);
    recheck_fires(&t);
    assert!(t.shown());
}

/// 伪离开计数在真实移动时清零（不经菜单关闭的 rearm）：此后又能再容忍满额的伪离开。
#[test]
fn real_move_resets_spurious_count() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    t.mouse
        .borrow_mut()
        .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
    for i in 0..MAX_SPURIOUS_LEAVES {
        mouse_leave(&t);
        assert!(t.shown(), "移动后第 {} 条伪离开仍留下", i + 1);
        recheck_fires(&t);
    }
}

/// `mouse_over` 已清（如菜单关闭判定隐藏）时 `hide()` 也撤掉复查，免得它到期后再动气泡。
#[test]
fn hide_without_mouse_over_cancels_recheck() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    assert!(t.mouse.borrow().recheck_pending);
    t.mouse_over.set(false);
    t.hide();
    assert!(!t.mouse.borrow().recheck_pending);
}

/// 抑制未开（关闭的是工具栏 / 状态等别人的菜单）时 `SetTooltipMenuOpen(false)` 不动气泡：
/// 光标不在气泡上也不隐藏、不改 `mouse_over` / 跟踪。
#[test]
fn foreign_menu_close_leaves_tip_alone() {
    let (mut t, _rx) = tooltip_with_rx(false);
    t.visible.set(true);
    t.mouse_over.set(true);
    t.mouse.borrow_mut().tracking = true;
    t.set_menu_open(false);
    assert!(t.shown());
    assert!(t.mouse_over.get());
    assert!(t.mouse.borrow().tracking);
}

/// 从气泡移回它所属的候选行（穿过空隙）：宽限到期时光标在候选行上 → 气泡留下，不挂
/// 跟踪（此后去留跟着候选悬停走）。mock 光标恒在 (0,0)。
#[test]
fn grace_expiry_on_anchor_keeps_tip() {
    let (mut t, _rx) = tooltip_with_rx(false);
    t.visible.set(true);
    t.set_anchor((-5, -5, 5, 5));
    t.mouse_over.set(true);
    mouse_leave(&t);
    recheck_fires(&t);
    assert!(t.shown(), "回到候选行：气泡留下");
    assert!(_rx.try_recv().is_err(), "回到候选行不清悬停");
    let m = t.mouse.borrow();
    assert!(!t.mouse_over.get() && !m.tracking && !m.recheck_pending);
}

/// 宽限期内候选侧决定换走（悬停变了 → `hide()`）：当场隐藏并撤掉宽限。
#[test]
fn hide_during_grace_hides_now() {
    let (mut t, _rx) = tooltip_with_rx(false);
    t.visible.set(true);
    t.mouse_over.set(true);
    mouse_leave(&t);
    t.hide();
    assert!(!t.shown());
    assert!(!t.mouse.borrow().recheck_pending);
}

/// 强制隐藏：光标就在气泡上、跟踪挂着也收；跟踪 / 复查 / 计数一并归位。对照 `hide()`
/// 在同样条件下推迟。
#[test]
fn hide_force_ignores_cursor_on_tip() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.simulate_move();
    t.hide();
    assert!(t.shown(), "对照：悬停类隐藏推迟");
    t.set_menu_open(true);
    t.set_menu_open(false);
    mouse_leave(&t);
    assert!(t.mouse.borrow().recheck_pending);
    t.hide_force();
    assert!(!t.shown());
    let m = t.mouse.borrow();
    assert!(!t.mouse_over.get() && !m.tracking && !m.recheck_pending && m.spurious == 0);
}

/// 候选窗先收（强制隐藏）、菜单后收（`HideCandidates` 的顺序）：已收起的气泡不再被
/// 「光标在气泡上 → 留下」重挂跟踪。
#[test]
fn menu_close_after_force_hide_does_not_rearm() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.hide_force();
    t.on_menu_dismissed();
    assert!(!t.shown());
    assert!(!t.mouse_over.get() && !t.mouse.borrow().tracking);
}

/// 同一次关闭收到第二条 `SetTooltipMenuOpen(false)`：幂等，不再重挂跟踪。
#[test]
fn repeated_menu_close_does_not_rearm() {
    let (mut t, _rx) = tooltip_with_rx(true);
    t.visible.set(true);
    t.set_menu_open(true);
    t.set_menu_open(false);
    assert!(t.mouse.borrow().tracking);
    // 伪离开清掉跟踪；随后到达的重复关闭不得再挂（再挂只会再招一条伪离开）。
    mouse_leave(&t);
    t.set_menu_open(false);
    assert!(!t.mouse.borrow().tracking, "重复关闭不重挂");
    assert!(t.shown());
}

/// 主题 `[tooltip] font_size` 是相对基准 13 的有符号偏移（与主题编辑器预览同语义）。
#[test]
fn theme_font_size_offset_applies_to_tooltip() {
    let mut t = tooltip();
    let base = t.renderer.base_size();
    let mut theme = wind_theme::Resolved::default();
    theme.views.tooltip = Some(wind_theme::RvNode {
        font_size: 4.0,
        ..Default::default()
    });
    t.set_theme(&theme);
    assert_eq!(t.renderer.base_size(), base + 4.0 * t.scale);
    // 换回无偏移的主题要回到基准，不能把上一份主题的字号粘住。
    t.set_theme(&wind_theme::Resolved::default());
    assert_eq!(t.renderer.base_size(), base);
}

/// 候选字号比参照大 / 小多少，气泡就跟着大 / 小多少，且与主题节点偏移叠加。
#[test]
fn candidate_font_delta_applies_to_tooltip() {
    let mut t = tooltip();
    let base = t.renderer.base_size();
    t.set_candidate_font(REF_CANDIDATE_PX + 6.0);
    assert_eq!(t.renderer.base_size(), base + 6.0 * t.scale);
    let mut theme = wind_theme::Resolved::default();
    theme.views.tooltip = Some(wind_theme::RvNode {
        font_size: -2.0,
        ..Default::default()
    });
    t.set_theme(&theme);
    assert_eq!(t.renderer.base_size(), base + 4.0 * t.scale);
    t.set_candidate_font(REF_CANDIDATE_PX);
    t.set_theme(&wind_theme::Resolved::default());
    assert_eq!(t.renderer.base_size(), base);
}

#[test]
fn client_point_is_signed_16_bit_pairs() {
    assert_eq!(client_point(LPARAM((20 << 16) | 10)), (10, 20));
    assert_eq!(client_point(LPARAM(0xFFFF_FFFF)), (-1, -1));
}

/// 片段按主题求色后接到气泡叶子：内联色那段走 draw_runs，气泡里名字先查 `tooltip_<名>`。
#[cfg(mock_text)]
#[test]
fn inline_color_reaches_the_tooltip_leaf() {
    use wind_ui_types::{SpanStyle, StyledText};
    let mut t = tooltip();
    let mut theme = wind_theme::Resolved::default();
    theme.palette.insert("error".into(), [1, 1, 1, 255]);
    theme.palette.insert("tooltip_error".into(), [2, 2, 2, 255]);
    t.set_theme(&theme);
    let mut text = StyledText::from("你：");
    text.push(
        "nǐ",
        &SpanStyle {
            color: Some(std::sync::Arc::new(wind_theme::InlineColor::parse("error"))),
            ..Default::default()
        },
    );
    let doc = TooltipDoc {
        sections: vec![TooltipSection {
            title: None,
            inline: false,
            lines: vec![TooltipLine { text, raw: 0 }],
        }],
    };
    let (_, _, _, log) = t.golden_frame(&Arc::new(doc));
    assert_eq!(log.len(), 1);
    assert!(
        log[0].starts_with("draw_runs ")
            && log[0].contains("ColorRun { start: 6, end: 9, rgba: [2, 2, 2, 255] }"),
        "{}",
        log[0]
    );
}

/// 非出厂测试主题的气泡角色色：段名装饰与字面走 title、段名里的变量回落 title、readings 自有色。
#[cfg(mock_text)]
#[test]
fn theme_roles_color_the_tooltip() {
    use wind_ui_types::{SpanStyle, StyledText};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let theme = wind_theme::load_resolved_dirs(
        &[
            root.join("../wind-theme/testdata/themes"),
            root.join("../../../data/themes"),
        ],
        "span-roles",
        false,
    )
    .unwrap();
    let mut t = tooltip();
    t.set_theme(&theme);
    let title_role = |s: &str, role| {
        let mut x = StyledText::new();
        x.push(
            s,
            &SpanStyle {
                role: Some(role),
                in_title: true,
                ..Default::default()
            },
        );
        x
    };
    let mut title = title_role("编码(", "title");
    title.append(&title_role("五笔", "code_source"));
    // `_base` 没配 dict：段名里的它回落 title——渲染层把 in_title 透传到求色的端到端覆盖。
    title.append(&title_role("释", "dict"));
    let mut line = StyledText::new();
    line.push(
        "hǎo",
        &SpanStyle {
            role: Some("readings"),
            ..Default::default()
        },
    );
    let doc = TooltipDoc {
        sections: vec![TooltipSection {
            title: Some(title),
            inline: false,
            lines: vec![TooltipLine { text: line, raw: 0 }],
        }],
    };
    let (_, _, _, log) = t.golden_frame(&Arc::new(doc));
    let accent = theme.palette["accent"];
    let info = theme.palette["tooltip_info"];
    // 整块文字是 "[编码(五笔释]\nhǎo"（样例段名只拼了「编码(」「五笔」「释」，没有右括号）：
    // 装饰 `[` 与字面 `编码(` 同为 title 角色、合成一段；`五笔` 是段名里的变量，有自己的角色色
    // （继承 `_base` 的 code_source = tooltip_info，§18）就不回落 title；`释`（dict，没配色）
    // 回落 title；装饰 `]` 是 title；换行不着色；readings 用本主题自有色（压过 `_base` 的）。
    let want = format!(
        "runs=[ColorRun {{ start: 0, end: 8, rgba: {accent:?} }}, ColorRun {{ start: 8, end: 14, rgba: {info:?} }}, \
             ColorRun {{ start: 14, end: 17, rgba: {accent:?} }}, ColorRun {{ start: 17, end: 18, rgba: {accent:?} }}, \
             ColorRun {{ start: 19, end: 23, rgba: [154, 208, 255, 255] }}]"
    );
    assert!(log[0].contains(&want), "{}\n期望含 {want}", log[0]);
}
