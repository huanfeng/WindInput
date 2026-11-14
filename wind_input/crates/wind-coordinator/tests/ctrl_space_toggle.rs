//! GH#172：关掉 Ctrl+空格 切换（`keys.ctrl_space_toggle = false`）后，**用户按出来的**
//! 系统中英切换（Windows 输入法开关热键翻 OPENCLOSE compartment，此刻 Ctrl 还按着）必须
//! 被拒绝，模式保持不变；回包携带当前模式，DLL 的 `_ApplyModeSwitch` 据此把 compartment
//! 拉回（与 `ignore_host_ime_switch` 同一条仲裁回路）。
//!
//! 宿主自己写 compartment（无 Ctrl）与功能菜单不受影响——它们不是 Ctrl+空格。
//! 不依赖词库（`new_headless(.., None)`）。

use wind_bridge::handler::MessageHandler;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::ModeSwitchSource as Src;

fn coord(toggle: bool, none_binding: bool) -> std::sync::Arc<Coordinator> {
    let mut c = Config::default();
    c.input.default.chinese_mode = true;
    c.keys.ctrl_space_toggle = toggle;
    if none_binding {
        c.keys
            .key_actions
            .insert("ctrl+space".into(), "none".into());
    }
    let coord = Coordinator::new_headless(c, None);
    assert!(coord.is_chinese_mode(), "前提：中文起步");
    coord
}

/// 关掉后，Ctrl 按住的 compartment 翻转被拒：模式不变、回包是当前模式（中文）。
#[test]
fn disabled_rejects_user_system_hotkey_flip() {
    for src in [
        Src::CompartmentOpenClose,
        Src::CompartmentConversion,
        Src::Unknown,
        Src::CtrlSpaceKey,
    ] {
        let c = coord(false, false);
        let (status, commit) = c.handle_system_mode_switch(false, src, true);
        assert!(c.is_chinese_mode(), "{src:?}+Ctrl 应被拒，仍是中文");
        assert!(
            status
                .expect("拒绝也要回包，DLL 靠它把 compartment 拉回")
                .chinese_mode,
            "{src:?} 回包须是当前模式"
        );
        assert!(commit.is_empty());
    }
}

/// 按键侧兜底（`CtrlSpaceKey`）没有 Ctrl 采样也算用户按的（与 `CapsCancelSwitch::from_system`
/// 同一判据）——旧版/竞态下 DLL 仍发了兜底切换，也不能翻。
#[test]
fn disabled_rejects_key_fallback_without_ctrl_sample() {
    let c = coord(false, false);
    c.handle_system_mode_switch(false, Src::CtrlSpaceKey, false);
    assert!(c.is_chinese_mode());
}

/// 「全局自定义按键」把 ctrl+space 绑成 none ⇒ 等同于关掉开关。
#[test]
fn key_actions_none_equals_switch_off() {
    let c = coord(true, true);
    c.handle_system_mode_switch(false, Src::CompartmentOpenClose, true);
    assert!(c.is_chinese_mode(), "绑成 none 后 Ctrl+空格 也不该切");
}

/// 宿主写 compartment（无 Ctrl）、功能菜单：不是 Ctrl+空格，照常落地。
#[test]
fn disabled_still_honours_host_and_menu() {
    let c = coord(false, false);
    c.handle_system_mode_switch(false, Src::CompartmentOpenClose, false);
    assert!(!c.is_chinese_mode(), "宿主关 IME（无 Ctrl）照办");

    let c = coord(false, false);
    c.handle_system_mode_switch(false, Src::Menu, false);
    assert!(!c.is_chinese_mode(), "菜单照办");
}

/// 出厂（开）：行为与以前一致，Ctrl+空格 照常切换。
#[test]
fn enabled_keeps_toggling() {
    let c = coord(true, false);
    c.handle_system_mode_switch(false, Src::CompartmentOpenClose, true);
    assert!(!c.is_chinese_mode(), "开着时 Ctrl+空格 照常切到英文");
    c.handle_system_mode_switch(true, Src::CtrlSpaceKey, false);
    assert!(c.is_chinese_mode(), "按键兜底照常切回中文");
}
