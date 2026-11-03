//! [`crate::handle_softkeyboard`] 的单元测试。
//!
//! 单独成文件（`#[path]` 挂载）只是为了不让主模块被测试撑长，它仍是 crate 内模块，
//! 照常访问 `pub(crate)` 项。

use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_ipc::protocol::{EVENT_KEY_DOWN, FocusLostReason};

/// `data_dir = None` ⇒ 软键盘表回落内置兜底（一面「标点」）。
fn coord() -> Arc<Coordinator> {
    let c = Coordinator::new_headless(Config::default(), None);
    declare_panels(&c);
    c
}

/// Linux 外部宿主形态：面板能力要 addon 声明（见 `Coordinator::host_has_panels`），这里的用例都
/// 假设宿主贴得出面板。未声明的那一面由 `linux_panel_tests::softkeyboard_needs_panels_declared_by_host` 测。
fn declare_panels(c: &Coordinator) {
    #[cfg(all(target_os = "linux", ext_presenter))]
    c.host_panels
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = c;
}

fn kev(vk: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn press(c: &Coordinator, vk: u32, modifiers: u32) -> Option<KeyAction> {
    let st = c.state.lock().unwrap_or_else(|e| e.into_inner());
    c.handle_softkeyboard_key(&st, &kev(vk, modifiers))
}

fn text_of(act: Option<KeyAction>) -> Option<String> {
    match act {
        Some(KeyAction::InsertText { text, .. }) => Some(text),
        _ => None,
    }
}

const VK_1: u32 = 0x31;
const VK_A: u32 = 0x41;
const VK_F1: u32 = 0x70;

#[test]
fn closed_by_default_and_takes_no_key() {
    let c = coord();
    assert!(!c.softkeyboard_is_open());
    assert!(press(&c, VK_1, 0).is_none(), "没开时一个键都不该接管");
}

#[test]
fn toggle_opens_and_closes() {
    let c = coord();
    c.toggle_softkeyboard(None);
    assert!(c.softkeyboard_is_open());
    c.toggle_softkeyboard(None);
    assert!(!c.softkeyboard_is_open());
}

/// 菜单里开面板：面板一开就接管主键区，正在打的码若还挂着，候选窗就成了「卡住」的残影。
/// 与热键入口同一道理（`softkeyboard_hotkey`），只是菜单没有按键应答，走 push 出口。
#[test]
fn menu_open_takes_pending_input() {
    let c = coord();
    c.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .input_buffer
        .push_str("gg");
    assert!(c.has_pending_composition());
    c.run_menu_cmd(wind_ui_types::MenuCmd::ToggleSoftKeyboard);
    assert!(c.softkeyboard_is_open());
    assert!(!c.has_pending_composition(), "开面板后不该残留编码");
}

/// 关面板方向不动输入态：软键盘态下本就没有输入会话。
#[test]
fn menu_close_does_not_touch_input() {
    let c = coord();
    c.toggle_softkeyboard(None);
    assert!(c.softkeyboard_is_open());
    c.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .input_buffer
        .push_str("gg");
    c.run_menu_cmd(wind_ui_types::MenuCmd::ToggleSoftKeyboard);
    assert!(!c.softkeyboard_is_open());
    assert!(c.has_pending_composition(), "关面板不该处置编码");
}

/// 漏掉这次推送的症状极隐蔽：Rust 接管按键，而 C++ 仍按「没开」判定不吃数字键，
/// 于是数字行整行失效、别的键都正常。
#[test]
fn open_and_close_mark_dirty_for_the_status_push() {
    let c = coord();
    c.softkeyboard_dirty.store(false, Ordering::Relaxed);
    c.open_softkeyboard(None);
    assert!(
        c.softkeyboard_dirty.load(Ordering::Relaxed),
        "开启必须置 dirty，否则 STATUS_SOFT_KEYBOARD 位推不出去"
    );
    c.softkeyboard_dirty.store(false, Ordering::Relaxed);
    c.close_softkeyboard();
    assert!(c.softkeyboard_dirty.load(Ordering::Relaxed), "关闭同理");
}

#[test]
fn status_carries_the_soft_keyboard_bit() {
    let c = coord();
    assert!(!c.build_status().soft_keyboard);
    c.open_softkeyboard(None);
    assert!(
        c.build_status().soft_keyboard,
        "状态位没带上，C++ 的吃键判定就看不到软键盘态"
    );
}

#[test]
fn symbol_slot_commits_from_the_current_page() {
    let c = coord();
    c.open_softkeyboard(None);
    // 内置兜底面「标点」：数字行第 2 个键位（VK_1）是 ！，字母行首 a 是 、
    assert_eq!(text_of(press(&c, VK_1, 0)).as_deref(), Some("！"));
    assert_eq!(text_of(press(&c, VK_A, 0)).as_deref(), Some("、"));
}

#[test]
fn shift_selects_the_second_layer() {
    let c = coord();
    c.open_softkeyboard(None);
    assert_eq!(text_of(press(&c, VK_1, 0)).as_deref(), Some("！"));
    assert_eq!(
        text_of(press(&c, VK_1, MOD_SHIFT)).as_deref(),
        Some("¡"),
        "按住 Shift 应取第二层"
    );
}

/// 特殊键与布局外按键的处置**不是一条规则，而是「没被拦下」的自然结果**。
/// 这条测的正是那个性质：不在布局表里 ⇒ 返回 None ⇒ 落回常规链路 ⇒ 透传。
#[test]
fn keys_outside_the_layout_are_not_taken() {
    let c = coord();
    c.open_softkeyboard(None);
    assert!(press(&c, VK_F1, 0).is_none(), "F1 不在布局里，不该接管");
    assert!(
        press(&c, keymap::VK_BACK, 0).is_none(),
        "退格是特殊键，必须透传——否则长按连删就没了"
    );
    assert!(press(&c, keymap::VK_RETURN, 0).is_none(), "回车同理");
    assert!(press(&c, keymap::VK_SPACE, 0).is_none(), "空格同理");
}

#[test]
fn shortcut_combos_stay_with_the_host() {
    let c = coord();
    c.open_softkeyboard(None);
    assert!(
        press(&c, VK_A, MOD_SHORTCUT).is_none(),
        "Ctrl/Alt 组合是宿主的快捷键，软键盘开着也不该抢"
    );
}

#[test]
fn escape_closes_the_panel() {
    let c = coord();
    c.open_softkeyboard(None);
    assert!(press(&c, keymap::VK_ESCAPE, 0).is_some(), "Esc 应被吃掉");
    assert!(!c.softkeyboard_is_open());
}

#[test]
fn page_keys_are_consumed() {
    let c = coord();
    c.open_softkeyboard(None);
    let before = c.softkeyboard_page_name();
    assert!(press(&c, keymap::VK_NEXT, 0).is_some(), "翻页键应被吃掉");
    // 内置兜底只有一面，循环一圈仍是它——重点是键被消费掉、不漏给宿主。
    assert_eq!(c.softkeyboard_page_name(), before);
}

/// 面不存在时若什么都不做，用户看到的就是「这个键没绑上」，与真的没绑完全同形。
#[test]
fn unknown_page_id_falls_back_to_plain_toggle() {
    let c = coord();
    c.toggle_softkeyboard(Some("no_such_page"));
    assert!(c.softkeyboard_is_open(), "面不存在也要至少把面板开出来");
}

/// 直通车语义：带面的绑定按第二次仍停在那一面，不能变成开关，
/// 否则给两个面各配一个直通键，两键会互相打架。
#[test]
fn direct_page_binding_does_not_toggle_off() {
    let c = coord();
    c.toggle_softkeyboard(Some("punct"));
    assert!(c.softkeyboard_is_open());
    c.toggle_softkeyboard(Some("punct"));
    assert!(c.softkeyboard_is_open(), "带面的绑定不该把已开着的面板关掉");
}

/// 跨宿主切换时旧宿主的 focus_lost 实测晚约 100ms 到达；不设守卫的表现是
/// 「点工具栏图标弹出即消失」。菜单为这件事踩过一轮。
#[test]
fn focus_guard_keeps_a_just_opened_panel() {
    let c = coord();
    c.open_softkeyboard(None);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Lost(FocusLostReason::Thread));
    assert!(c.softkeyboard_is_open(), "守卫期内的焦点事件应被忽略");
}

#[test]
fn focus_change_closes_after_the_guard() {
    let c = coord();
    c.open_softkeyboard(None);
    *c.softkeyboard_opened_at
        .lock()
        .unwrap_or_else(|e| e.into_inner()) =
        Some(std::time::Instant::now() - crate::handle_menu::MENU_FOCUS_GUARD * 2);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Lost(FocusLostReason::Thread));
    assert!(!c.softkeyboard_is_open(), "守卫期外应关闭");
}

/// 把守卫期拨过去，让后面的焦点事件按真实路径判。
fn expire_focus_guard(c: &Coordinator) {
    *c.softkeyboard_opened_at
        .lock()
        .unwrap_or_else(|e| e.into_inner()) =
        Some(std::time::Instant::now() - crate::handle_menu::MENU_FOCUS_GUARD * 2);
}

/// Excel 里退出单元格是 CtxLost / NoEditCtx、换文档是 DocChanged：都是同宿主内的噪声，
/// 连续录入不能被它们打断。
#[test]
fn same_host_focus_noise_keeps_the_panel() {
    for reason in [
        FocusLostReason::CtxLost,
        FocusLostReason::NoEditCtx,
        FocusLostReason::DocChanged,
    ] {
        let c = coord();
        c.open_softkeyboard(None);
        expire_focus_guard(&c);
        c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Lost(reason));
        assert!(c.softkeyboard_is_open(), "{reason:?} 不该关面板");
    }
}

/// 点进下一个单元格 = 同进程的 focus_gained，同样不算离开。
#[test]
fn same_host_focus_gained_keeps_the_panel() {
    let c = coord();
    c.open_softkeyboard(None);
    c.softkeyboard_host_pid.store(1234, Ordering::Relaxed);
    expire_focus_guard(&c);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 1234 });
    assert!(c.softkeyboard_is_open());
}

#[test]
fn other_host_focus_gained_closes_the_panel() {
    let c = coord();
    c.open_softkeyboard(None);
    c.softkeyboard_host_pid.store(1234, Ordering::Relaxed);
    expire_focus_guard(&c);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 5678 });
    assert!(!c.softkeyboard_is_open(), "换了宿主进程应关闭");
}

#[test]
fn thread_lost_and_ime_deactivated_close_the_panel() {
    for sig in [
        SoftKbFocusSignal::Lost(FocusLostReason::Thread),
        SoftKbFocusSignal::ImeDeactivated,
    ] {
        let c = coord();
        c.open_softkeyboard(None);
        expire_focus_guard(&c);
        c.close_softkeyboard_on_focus_change(sig);
        assert!(!c.softkeyboard_is_open(), "{sig:?} 应关闭");
    }
}

/// 宿主未知（开启时没有活动宿主）或事件取不到 pid：保守不关，用户可手动收掉。
#[test]
fn unknown_pid_never_closes_on_gained() {
    for (host, pid) in [(0, 5678), (1234, 0)] {
        assert!(
            !Coordinator::softkeyboard_should_close_on(SoftKbFocusSignal::Gained { pid }, host),
            "host={host} pid={pid}"
        );
    }
}

/// 钉住后连整应用失焦、换宿主都不关；这是它存在的全部意义。
#[test]
fn pinned_panel_survives_every_focus_signal() {
    let c = coord();
    c.open_softkeyboard(None);
    c.softkeyboard_host_pid.store(1234, Ordering::Relaxed);
    assert!(c.toggle_softkeyboard_pin());
    expire_focus_guard(&c);
    for sig in [
        SoftKbFocusSignal::Lost(FocusLostReason::Thread),
        SoftKbFocusSignal::Gained { pid: 5678 },
        SoftKbFocusSignal::ImeDeactivated,
    ] {
        c.close_softkeyboard_on_focus_change(sig);
        assert!(c.softkeyboard_is_open(), "钉住后 {sig:?} 不该关");
    }
}

#[test]
fn toggling_pin_twice_unpins() {
    let c = coord();
    c.open_softkeyboard(None);
    assert!(c.toggle_softkeyboard_pin());
    assert!(!c.toggle_softkeyboard_pin());
    expire_focus_guard(&c);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Lost(FocusLostReason::Thread));
    assert!(!c.softkeyboard_is_open(), "取消钉住后恢复自动关闭");
}

/// 钉住只对本次打开有效：手动关掉再开，不能还钉着。
#[test]
fn pin_does_not_survive_a_close() {
    let c = coord();
    c.open_softkeyboard(None);
    c.toggle_softkeyboard_pin();
    c.close_softkeyboard();
    c.open_softkeyboard(None);
    assert!(!c.softkeyboard_pinned.load(Ordering::Relaxed));
}

/// 钉住期间跟着用户去了别的应用，取消钉住后那个应用就是宿主：
/// 在它里面换焦点不能关（否则「同宿主噪声不关」在新应用里又失效）。
#[test]
fn unpinning_in_another_app_adopts_it_as_host() {
    let c = coord();
    c.open_softkeyboard(None);
    c.softkeyboard_host_pid.store(1234, Ordering::Relaxed);
    c.toggle_softkeyboard_pin();
    expire_focus_guard(&c);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 5678 });
    c.toggle_softkeyboard_pin(); // 取消钉住
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 5678 });
    assert!(c.softkeyboard_is_open(), "新应用内的换焦点不该关");
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 1234 });
    assert!(!c.softkeyboard_is_open(), "再换回旧应用才算离开");
}

/// 开启时没有活动宿主（pid 0）：第一次获焦补记，之后同宿主不关、换宿主才关。
#[test]
fn unknown_host_is_adopted_on_first_gained() {
    let c = coord();
    c.open_softkeyboard(None);
    c.softkeyboard_host_pid.store(0, Ordering::Relaxed);
    expire_focus_guard(&c);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 42 });
    assert!(c.softkeyboard_is_open());
    assert_eq!(c.softkeyboard_host_pid.load(Ordering::Relaxed), 42);
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 42 });
    assert!(c.softkeyboard_is_open());
    c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Gained { pid: 43 });
    assert!(!c.softkeyboard_is_open());
}

/// 竞态兜底：关闭后残留的钉住标志，在下次打开时必须清掉。
#[test]
fn open_always_starts_unpinned() {
    let c = coord();
    c.softkeyboard_pinned.store(true, Ordering::Relaxed);
    c.open_softkeyboard(None);
    assert!(!c.softkeyboard_pinned.load(Ordering::Relaxed));
}

/// 开启时按活动 token 高 32 位记宿主 pid——接线本身，不是手工 store。
#[test]
fn open_records_host_pid_from_active_token() {
    let c = coord();
    c.push_server.set_active_token((777u64 << 32) | 5);
    c.open_softkeyboard(None);
    assert_eq!(c.softkeyboard_host_pid.load(Ordering::Relaxed), 777);
}

#[test]
fn pin_toggle_tells_the_ui() {
    let (c, rx) = coord_with_ui();
    c.open_softkeyboard(None);
    while rx.try_recv().is_ok() {}
    c.toggle_softkeyboard_pin();
    let cmds: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        cmds.iter()
            .any(|m| matches!(m, UiCommand::SoftKeyboardPinned(true))),
        "应通知 UI 钉住: {cmds:?}"
    );
}

#[test]
fn pin_is_ignored_while_closed() {
    let c = coord();
    assert!(!c.toggle_softkeyboard_pin());
    assert!(!c.softkeyboard_pinned.load(Ordering::Relaxed));
}

#[test]
fn reopening_stays_on_the_last_page() {
    let c = coord();
    c.open_softkeyboard(None);
    let page = c.softkeyboard_page_idx();
    c.close_softkeyboard();
    c.open_softkeyboard(None);
    assert_eq!(c.softkeyboard_page_idx(), page, "再开应停在同一面");
}

/// 布局里的键位必须都能从虚拟键码反查到，否则那个键位在面板上画着符号却敲不出。
#[test]
fn vk_map_covers_every_slot_in_the_layout() {
    assert_eq!(
        vk_to_slot().len(),
        wind_softkeyboard::SLOT_COUNT,
        "有键位没建立 vk 映射"
    );
}

// ── 关闭路径与工具栏图标的同步 ───────────────────────────────────

/// 带 UI 通道的夹具：要验证「工具栏刷新了」只能读真实发出去的命令。
///
/// ⚠️ 必须把 `notify_toolbar` 那道三项合取（本输入法在服务某宿主 / 焦点在可编辑控件里 /
/// 用户开着工具栏）全部置真，否则它走的是隐藏分支、只发 `HideToolbar`，一条
/// `UpdateToolbar` 也见不到——那样这个测试断言的就成了「工具栏没显示」，与要验的事无关。
fn coord_with_ui() -> (
    Arc<Coordinator>,
    std::sync::mpsc::Receiver<wind_ui_types::UiCommand>,
) {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    declare_panels(&c);
    {
        let mut s = c.state.lock().unwrap_or_else(|e| e.into_inner());
        s.ime_active = true;
        s.has_edit_context = true;
        s.toolbar_visible = true;
    }
    (c, rx)
}

/// 排空通道，返回最后一次 `UpdateToolbar` 里的 `soft_keyboard_on`（没推过则 None）。
fn last_toolbar_soft_kb(rx: &std::sync::mpsc::Receiver<wind_ui_types::UiCommand>) -> Option<bool> {
    let mut last = None;
    while let Ok(cmd) = rx.try_recv() {
        if let wind_ui_types::UiCommand::UpdateToolbar(st) = cmd {
            last = Some(st.soft_keyboard_on);
        }
    }
    last
}

/// ★ 本次修的那条：**每一条关闭路径都要把工具栏图标灭掉**。
///
/// 关闭软键盘有五条路（Esc、面板关闭按钮、热键、工具栏点击、菜单、失焦），此前只有
/// 工具栏与菜单两条记得刷工具栏 ⇒ 用 Esc 或面板上的关闭按钮关掉后，**图标一直亮着**。
///
/// 断言读的是**真的发给 UI 的命令**而不是 `softkeyboard_is_open()`：后者是收口函数的
/// 输入，收口写好而调用点漏接时它照样对——那正是这个 bug 的形态。逐条路径过一遍，
/// 加第六条路而忘了收口就会红。
#[test]
fn every_close_path_clears_the_toolbar_icon() {
    // (路径名, 关闭动作)。开启一律走 open_softkeyboard，只变关闭这一侧。
    #[allow(clippy::type_complexity)]
    let paths: Vec<(&str, Box<dyn Fn(&Coordinator)>)> = vec![
        (
            "Esc",
            Box::new(|c: &Coordinator| {
                let st = c.state.lock().unwrap_or_else(|e| e.into_inner());
                let act = c.handle_softkeyboard_key(&st, &kev(keymap::VK_ESCAPE, 0));
                drop(st);
                assert!(act.is_some(), "Esc 应被面板接管");
                // 按键路径上由 SoftKeyboardPushOnDrop 收口，这里手动跑一次它的内容
                // （测试直接调 handle_softkeyboard_key，没有经过 handle_key_event 顶层）。
                if c.softkeyboard_dirty.swap(false, Ordering::Relaxed) {
                    c.after_softkeyboard_change();
                }
            }),
        ),
        (
            "面板关闭按钮",
            Box::new(|c: &Coordinator| c.ui_softkeyboard_close()),
        ),
        (
            "失焦",
            Box::new(|c: &Coordinator| {
                *c.softkeyboard_opened_at
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(std::time::Instant::now() - crate::handle_menu::MENU_FOCUS_GUARD * 2);
                c.close_softkeyboard_on_focus_change(SoftKbFocusSignal::Lost(
                    FocusLostReason::Thread,
                ));
            }),
        ),
        (
            "工具栏点击",
            Box::new(|c: &Coordinator| {
                c.mouse_toolbar(wind_ui_types::ToolbarAction::ToggleSoftKeyboard)
            }),
        ),
        (
            "菜单",
            Box::new(|c: &Coordinator| {
                c.menu_action(wind_ui_types::MenuKind::Command(
                    wind_ui_types::MenuCmd::ToggleSoftKeyboard,
                ))
            }),
        ),
    ];
    for (name, close) in paths {
        let (c, rx) = coord_with_ui();
        c.open_softkeyboard(None);
        c.after_softkeyboard_change();
        assert_eq!(
            last_toolbar_soft_kb(&rx),
            Some(true),
            "{name}: 前置——开启后图标该亮"
        );
        close(&c);
        assert!(!c.softkeyboard_is_open(), "{name}: 应该已经关掉了");
        assert_eq!(
            last_toolbar_soft_kb(&rx),
            Some(false),
            "{name}: 关掉了但工具栏没收到新状态，图标会一直亮着"
        );
    }
}

/// 该记什么：记**面 id**（不是下标）、开着时不记、同一面不重复记。
///
/// 测的是 `softkeyboard_page_to_persist` 这个纯判据，而不是 `save_softkeyboard_page`——
/// 后者要写进程外的全局 `state.toml`（`%LOCALAPPDATA%\WindInput[Dev]`），测试碰不得：
/// 那会改掉开发者本机的记录，还会与**正在运行的服务**抢同一个文件（两边都是
/// load-modify-save，一次丢更新就能吞掉刚存好的 `toolbar_positions`）。
/// 判据与写盘因此分成两层，下面 `headless_never_touches_the_real_state_toml` 守另一半。
#[test]
fn page_to_persist_uses_the_id_and_skips_redundant_writes() {
    let c = coord();
    let Some(first) = c.softkeyboard.pages().first().map(|p| p.id.clone()) else {
        return; // 兜底表为空（不该发生），无从验证
    };
    let set_mirror = |v: &str| {
        *c.softkeyboard_page_saved
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = v.to_string();
    };
    set_mirror("<从未记录过的面>");

    // 开着时一律不记：翻页键走系统 auto-repeat 且刻意不去抖，开着时也记就成了在按键
    // 返回路径上每秒几十次重写整个 state.toml。
    c.open_softkeyboard(None);
    assert_eq!(
        c.softkeyboard_page_to_persist(),
        None,
        "面板开着时不该落盘（长按翻页会打爆按键路径）"
    );

    // 关掉才记，记的是面 id 而不是下标。
    c.close_softkeyboard();
    assert_eq!(
        c.softkeyboard_page_to_persist(),
        Some(first.clone()),
        "关掉后该记的是面 id"
    );

    // 镜像已是这一面 ⇒ 不重复写。
    set_mirror(&first);
    assert_eq!(c.softkeyboard_page_to_persist(), None, "同一面不该重复记录");
}

/// ⚠️ **测试进程绝不写本机的 `state.toml`**。
///
/// `Config::state_dir()` 是进程外的全局路径，而单测构造的协调器同样走得到落盘那条路。
/// 门开着的后果有两个，都不像是测试的问题：跑一次 `cargo test` 改掉开发者自己机器上的
/// `last_softkeyboard_page`；以及与正在运行的服务抢同一个文件，丢更新时连
/// `toolbar_positions` 一起吞掉。
///
/// 判据借 `store`（它的文档本来就写着「None = 无持久化（headless 测试）」），
/// 那道门如今收在 `StateWriter::new(store.is_some(), ..)` 里，本条钉的是它还在。
///
/// ⚠️ **判据换过一次，别改回去**。原先问的是「`softkeyboard_page_saved` 镜像有没有变」，
/// 拿副作用代理「写盘路径有没有走」。门下沉到写入器之后镜像会照常更新（进程内状态，
/// 无害），那个代理就失效了——更糟的是它失效时报的错是「测试正在改开发者本机的
/// state.toml」，把人引向一个根本不存在的问题。现在直接问写入器本身有没有落盘能力。
#[test]
fn headless_never_touches_the_real_state_toml() {
    let c = coord();
    assert!(c.store.is_none(), "前置：headless 夹具无 store");
    assert!(
        c.state_writer.is_noop(),
        "headless 夹具的 state_writer 竟能落盘 ⇒ 跑一次 cargo test 就会改掉开发者本机的 \
         state.toml，还会与正在运行的服务抢同一个文件"
    );
    c.open_softkeyboard(None);
    c.after_softkeyboard_change();
    c.close_softkeyboard();
    c.after_softkeyboard_change(); // 判据说「该写」，但写入器该是空实现
    // 前置：确认这条路真的走到了落盘登记那一步（镜像已更新为当前面），
    // 否则上面那条 `is_noop` 断言测的就只是构造形态，与软键盘无关。
    let current = c
        .softkeyboard
        .pages()
        .get(c.softkeyboard_page_idx())
        .map(|p| p.id.clone())
        .unwrap_or_default();
    assert_eq!(
        *c.softkeyboard_page_saved
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
        current,
        "前置：关闭软键盘应已登记当前面（登记走的是空实现写入器，不落盘）"
    );
}

/// ⛔ 小键盘**不能**进软键盘键位表——这条钉的是跨语言的不变量。
///
/// # 它防的是什么
///
/// C++ 侧 `_IsSoftKeyboardEatenKey` 在符号面上吃 `HotkeyType::Number`，而
/// `ClassifyInputKey` 把 VK_NUMPAD0-9 与 + - * / . 一并归成 `Number`。若这张表里也
/// 没有小键盘，那就是「C++ 吃了、Rust 出不了字」——落回常规链路后英文半角态
/// PassThrough，严格宿主（Chrome/Electron）不补合成 WM_CHAR ⇒ **数字凭空消失，
/// 且两侧都没有日志**。0.120 周期真出过这个 bug。
///
/// 修法是让 C++ 放行小键盘（`_IsSoftKeyboardOutOfLayoutKey`），前提是这张表确实
/// 不含它们。⇒ **哪天往布局里加了小键盘键位，本测试会红**：那时必须同步去掉 C++
/// 侧那道放行，否则新加的键位敲下去没反应（这次是反方向的同一个坑）。
///
/// 判据写成「表里没有这些 vk」而不是「布局里没有这些键名」：真正决定吃不吃的是 vk。
#[test]
fn numpad_stays_out_of_the_slot_table_so_the_cpp_side_can_pass_it_through() {
    // VK_NUMPAD0-9 = 0x60..=0x69；VK_MULTIPLY/ADD/SUBTRACT/DECIMAL/DIVIDE = 0x6A..=0x6F。
    // 与 wind_tsf/include/KeyEventSink.h 的 _IsSoftKeyboardOutOfLayoutKey 同一组。
    let map = vk_to_slot();
    for vk in 0x60u32..=0x6F {
        assert!(
            !map.contains_key(&vk),
            "vk 0x{vk:02X} 进了软键盘键位表（{:?}）——C++ 侧仍在放行小键盘，\
             这个键位敲下去不会有反应。加小键盘布局时须同步改 _IsSoftKeyboardOutOfLayoutKey",
            map.get(&vk)
        );
    }
    // 反向前置：主键盘数字确实在表里，否则上面那圈断言可能只是因为表是空的。
    assert!(
        map.contains_key(&0x35),
        "前置：主键盘 5（vk 0x35）应在键位表里"
    );
}

/// Linux 外部宿主形态：软键盘面板由服务端光栅化、addon 贴图，开启路径与 Windows 同一条。
#[cfg(all(target_os = "linux", ext_presenter))]
mod linux_host {
    use super::*;
    use wind_bridge::handler::MessageHandler;
    use wind_ipc::protocol::FocusLostReason;
    use wind_ui_types::UiCommand;

    /// 当前面上第一个有符号的键位及其上屏文本。
    fn first_symbol_slot(c: &Coordinator) -> (&'static str, String) {
        let page = &c.softkeyboard.pages()[c.softkeyboard_page_idx()];
        wind_softkeyboard::all_slots()
            .find_map(|slot| page.output(slot, false).map(|t| (slot, t.to_string())))
            .expect("前置：兜底面至少有一个符号键位")
    }

    /// 出厂热键 ctrl+shift+k 开得出面板，主键区随之接管（会话位与 C++ 吃键判定同步置位）。
    #[test]
    fn open_softkeyboard_works_on_linux_host() {
        let (c, rx) = coord_with_ui();
        assert!(matches!(c.open_softkeyboard(None), KeyAction::Consumed));
        assert!(c.softkeyboard_is_open());
        assert!(
            rx.try_iter()
                .any(|m| matches!(m, UiCommand::ShowSoftKeyboard { .. })),
            "开启要把面板下发给渲染端"
        );
        assert!(
            c.build_status().soft_keyboard,
            "状态位要告诉 addon 吃主键区"
        );
        assert!(
            text_of(press(&c, VK_1, 0)).is_some(),
            "主键区的符号键位要被接管上屏"
        );
        assert!(matches!(c.toggle_softkeyboard(None), KeyAction::Consumed));
        assert!(!c.softkeyboard_is_open(), "再按一次关掉");
    }

    /// 面板上点符号键帽：经 push 出口上屏（UI 事件不在按键路径上，没有按键应答可搭）。
    #[test]
    fn softkeyboard_symbol_click_commits() {
        let (c, _rx) = coord_with_ui();
        c.open_softkeyboard(None);
        assert!(c.softkeyboard_is_open(), "前置：面板开得出来");
        let (slot, text) = first_symbol_slot(&c);
        let cap = c.push_server.attach_capture_client(7);
        c.ui_softkeyboard_key(slot, false, false);
        let want = wind_ipc::codec::encode_commit_text(&text, None, false, true, false);
        let got: Vec<Vec<u8>> = cap.try_iter().collect();
        assert!(
            got.contains(&want),
            "点 {slot} 应上屏 {text:?}，实际推送 {} 帧",
            got.len()
        );
    }

    /// 语义照 Windows：失焦（过了守卫期）**关闭**面板，而不是只藏起来——主键区接管跟着
    /// 解除，C++ 侧会话位清掉，物理键回到常规链路；切回来也不会自己弹出。
    #[test]
    fn softkeyboard_focus_lost_releases_keys() {
        let (c, rx) = coord_with_ui();
        c.open_softkeyboard(None);
        c.after_softkeyboard_change();
        *c.softkeyboard_opened_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) =
            Some(std::time::Instant::now() - crate::handle_menu::MENU_FOCUS_GUARD * 2);
        let _ = rx.try_iter().count();
        c.handle_focus_lost(1, FocusLostReason::Thread);
        assert!(!c.softkeyboard_is_open(), "失焦要关闭软键盘");
        assert!(
            rx.try_iter()
                .any(|m| matches!(m, UiCommand::HideSoftKeyboard)),
            "面板要收起，否则屏上留着一个点了没反应的面板"
        );
        assert!(
            !c.build_status().soft_keyboard,
            "会话位要清，addon 不再吃主键区"
        );
        assert!(press(&c, VK_1, 0).is_none(), "物理键交还常规输入链路");
    }
}
