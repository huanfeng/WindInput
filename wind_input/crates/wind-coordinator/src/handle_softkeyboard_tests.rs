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
