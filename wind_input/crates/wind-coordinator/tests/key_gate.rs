//! 吃键判定的契约测试。
//!
//! # 为什么这些用例必须存在
//!
//! 「该不该吃这个键」判错的表现是**统一而隐蔽**的：协调器对不该收的键返回
//! `Consumed`（意为「已在输入法内处理」），宿主当成消费后既不上屏也不执行默认行为
//! ——键静默消失，不报错、不崩溃、日志也正常。
//!
//! 这个形态在 Android 接入过程中一个会话里出现了**三次**（空缓冲功能键、英文模式字母，
//! 以及标点/翻页这些还没暴露的），根因都是宿主手写的判据与核心漂移。判据收进
//! `Coordinator::should_handle_key` 之后，本文件就是它的契约。
//!
//! 每加一类键，先在这里加断言。

use std::path::PathBuf;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_host::{KeyProbe, Modifiers};
use wind_keys::keymap;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
}

fn coordinator() -> Option<std::sync::Arc<Coordinator>> {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return None;
    }
    let mut cfg = Config::load(Some(&data_dir())).unwrap_or_default();
    cfg.schema.active = "wubi86".into();
    cfg.schema.available = vec!["wubi86".into()];
    Some(Coordinator::new_headless(cfg, Some(&data_dir())))
}

const VK_A: u32 = 0x41;
const VK_1: u32 = 0x31;

fn probe(vk: u32) -> KeyProbe {
    KeyProbe::new(vk)
}

/// Shift+数字是上挡标点，**空缓冲时也要吃**。
///
/// 现象（已修）：中文标点态下 `!@#$%^&*()` 整排绕过标点层直出半角，而且只在空缓冲时如此
/// ——有编码时又是全角，看起来像「有时候转有时候不转」。根因是 `is_session_only_key`
/// 只看 VK 不看修饰位，把 Shift+1 也当成了「数字键，空缓冲交还宿主」。
#[test]
fn shifted_digits_are_punct_even_on_empty_buffer() {
    let Some(c) = coordinator() else { return };
    for vk in 0x30u32..=0x39 {
        let mut p = KeyProbe::new(vk);
        p.modifiers = Modifiers(0x0001); // MOD_SHIFT
        assert!(
            c.should_handle_key(&p),
            "Shift+{:#x} 是上挡标点，空缓冲也应被吃",
            vk
        );
        // 不带 Shift 的数字仍旧交还宿主（原有契约不能被这条改坏）
        assert!(
            !c.should_handle_key(&probe(vk)),
            "{vk:#x} 无 Shift 时空缓冲应交还宿主"
        );
    }
}

/// 空缓冲下的功能键/数字必须交还宿主。
///
/// 设备现象（已修）：空格打不出空格、回车不换行、退格删不掉字、数字打不出来。
#[test]
fn empty_buffer_function_keys_go_to_host() {
    let Some(c) = coordinator() else { return };
    for (name, vk) in [
        ("空格", keymap::VK_SPACE),
        ("回车", keymap::VK_RETURN),
        ("退格", keymap::VK_BACK),
        ("Esc", keymap::VK_ESCAPE),
        ("左", keymap::VK_LEFT),
        ("右", keymap::VK_RIGHT),
        ("数字1", VK_1),
    ] {
        assert!(
            !c.should_handle_key(&probe(vk)),
            "空缓冲下「{name}」应交还宿主",
        );
    }
}

/// 有组合时，同一批键改由输入法承担语义（上屏/取消/删码/翻页/选词）。
#[test]
fn composing_function_keys_are_eaten() {
    let Some(c) = coordinator() else { return };
    // 先建立组合
    assert!(c.should_handle_key(&probe(VK_A)), "中文模式字母应被吃");
    feed(&c, VK_A);
    assert!(c.is_composing(), "前置条件：应已建立组合");

    for (name, vk) in [
        ("空格", keymap::VK_SPACE),
        ("回车", keymap::VK_RETURN),
        ("退格", keymap::VK_BACK),
        ("数字1", VK_1),
    ] {
        assert!(
            c.should_handle_key(&probe(vk)),
            "有组合时「{name}」应由输入法处理",
        );
    }
}

/// 英文模式下字母交还宿主。
///
/// 设备现象（已修）：切到英文就打不出字母——核心返回 Consumed 而无文本，宿主两头落空。
#[test]
fn english_mode_letters_go_to_host() {
    let Some(c) = coordinator() else { return };
    assert!(
        c.should_handle_key(&probe(VK_A)),
        "前置条件：中文模式吃字母"
    );

    // 走公开的命令通道切英文（与 UI/Android 同一条路）
    c.inject_ui_event(wind_ui_types::UiEvent::MenuAction(
        wind_ui_types::MenuKind::Command(wind_ui_types::MenuCmd::SchemaEnglish),
    ));
    assert!(!c.is_chinese_mode(), "前置条件：已切到英文模式");

    for vk in [VK_A, 0x42, 0x5A] {
        assert!(
            !c.should_handle_key(&probe(vk)),
            "英文模式下字母 0x{vk:X} 应交还宿主",
        );
    }
}

/// 中文模式下标点要吃（要转中文标点）。
#[test]
fn chinese_mode_eats_punctuation() {
    let Some(c) = coordinator() else { return };
    for (name, vk) in [
        ("逗号", keymap::VK_COMMA),
        ("句号", keymap::VK_PERIOD),
        ("分号", keymap::VK_SEMICOLON),
        ("引号", keymap::VK_QUOTE),
    ] {
        assert!(
            c.should_handle_key(&probe(vk)),
            "中文模式下「{name}」应被吃（要转中文标点）",
        );
    }
}

/// Ctrl/Alt 组合未命中热键时归宿主，别吃掉 Ctrl+C。
#[test]
fn ctrl_alt_combos_go_to_host() {
    let Some(c) = coordinator() else { return };
    let ctrl = Modifiers(Modifiers::CTRL);
    let alt = Modifiers(Modifiers::ALT);
    assert!(
        !c.should_handle_key(&probe(0x43).with_modifiers(ctrl)),
        "Ctrl+C 应交还宿主",
    );
    assert!(
        !c.should_handle_key(&probe(VK_A).with_modifiers(alt)),
        "Alt+A 应交还宿主",
    );
}

/// 宿主报只读上下文时一个键都不吃。
#[test]
fn readonly_context_eats_nothing() {
    let Some(c) = coordinator() else { return };
    for vk in [VK_A, keymap::VK_COMMA, keymap::VK_SPACE] {
        assert!(
            !c.should_handle_key(&probe(vk).readonly(true)),
            "只读上下文下 0x{vk:X} 应交还宿主",
        );
    }
}

/// 吃键判定与实际处理必须一致：**凡是判定吃的键，处理后不能既消费又无输出**。
///
/// 这条是上面所有用例的总闸门——它直接断言那个反复出现的 bug 形态不存在。
#[test]
fn eaten_keys_always_produce_output() {
    let Some(c) = coordinator() else { return };
    let mut checked = 0;
    for vk in [VK_A, keymap::VK_COMMA, keymap::VK_PERIOD] {
        if !c.should_handle_key(&probe(vk)) {
            continue;
        }
        let action = feed(&c, vk);
        assert!(
            !matches!(action, wind_bridge::handler::KeyAction::Consumed),
            "0x{vk:X} 被判定为吃，却返回 Consumed（既消费又无输出）",
        );
        checked += 1;
    }
    assert!(checked > 0, "一个键都没验到，用例失去意义");
}

fn feed(c: &Coordinator, vk: u32) -> wind_bridge::handler::KeyAction {
    use wind_bridge::handler::{KeyEventData, MessageHandler};
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    })
}
