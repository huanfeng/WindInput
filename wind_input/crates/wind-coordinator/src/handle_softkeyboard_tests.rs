//! [`crate::handle_softkeyboard`] 的单元测试。
//!
//! 单独成文件（`#[path]` 挂载）只是为了不让主模块被测试撑长，它仍是 crate 内模块，
//! 照常访问 `pub(crate)` 项。

use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_ipc::protocol::EVENT_KEY_DOWN;

/// `data_dir = None` ⇒ 软键盘表回落内置兜底（一面「标点」）。
fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
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
    c.close_softkeyboard_on_focus_change("focus_lost");
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
    c.close_softkeyboard_on_focus_change("focus_lost");
    assert!(!c.softkeyboard_is_open(), "守卫期外应关闭");
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
                c.close_softkeyboard_on_focus_change("focus_lost");
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
/// 判据借 `store`（它的文档本来就写着「None = 无持久化（headless 测试）」）。
/// 这条钉的是那道门还在——镜像没被改动即证明写盘整条路没走。
#[test]
fn headless_never_touches_the_real_state_toml() {
    let c = coord();
    assert!(c.store.is_none(), "前置：headless 夹具无 store");
    *c.softkeyboard_page_saved
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = "<哨兵>".to_string();
    c.open_softkeyboard(None);
    c.after_softkeyboard_change();
    c.close_softkeyboard();
    c.after_softkeyboard_change(); // 判据说「该写」，但 store 门该拦下
    assert_eq!(
        *c.softkeyboard_page_saved
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
        "<哨兵>",
        "镜像变了 ⇒ 写盘那条路真的走了 ⇒ 测试正在改开发者本机的 state.toml"
    );
    assert!(
        c.softkeyboard_page_to_persist().is_some(),
        "前置：判据本身说该写，否则上面那条断言恒真"
    );
}
