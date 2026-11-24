//! 吃键判定的契约测试。
//!
//! 「该不该吃这个键」判错的表现统一而隐蔽：协调器对不该收的键返回 `Consumed`，宿主当成已消费，
//! 既不上屏也不执行默认行为，键静默消失、不报错、日志也正常。根因常是宿主手写的判据与核心漂移，
//! 判据收进 `Coordinator::should_handle_key` 之后，本文件就是它的契约。
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

/// Shift+数字的上挡标点：会转换的吃，产物原样的透传。吃与不吃取决于这个键在当前配置下会不会被改写。
///
/// 会转换的那半：`is_session_only_key` 若只看 VK 不看修饰位，会把 Shift+1 当成「数字键，空缓冲交还
/// 宿主」，中文标点态下 `!@#$%^&*()` 在空缓冲时绕过标点层直出半角。
///
/// 产物原样的那半（`@#%&*`，中文标点表里没有映射）必须透传：吃了再吐回，非 TSF-aware 宿主会把字符码
/// 当虚拟键码解释（`#`→VK_END 等），见 docs/architecture/smart-symbol-compat-notes.md 的
/// 「产物不变的标点要透传，不能吃了再原样吐回」。
#[test]
fn shifted_digits_eaten_only_when_they_convert() {
    let Some(c) = coordinator() else { return };

    // 有中文标点映射 ⇒ 要转换 ⇒ 必须吃。(VK, 产物, 中文形)
    for (vk, ch, cn) in [
        (0x30u32, ')', '）'),
        (0x31, '!', '！'),
        (0x34, '$', '￥'),
        (0x36, '^', '…'),
        (0x39, '(', '（'),
    ] {
        let mut p = KeyProbe::new(vk);
        p.modifiers = Modifiers(0x0001); // MOD_SHIFT
        assert!(
            c.should_handle_key(&p),
            "Shift+{vk:#x} 出 `{ch}`→`{cn}`，要转换，空缓冲也应被吃"
        );
    }

    // 中文标点表无映射 ⇒ 产物即原样 ⇒ 必须透传（否则就是 B-9 那条「吃了再吐」）。
    for (vk, ch) in [
        (0x32u32, '@'),
        (0x33, '#'),
        (0x35, '%'),
        (0x37, '&'),
        (0x38, '*'),
    ] {
        let mut p = KeyProbe::new(vk);
        p.modifiers = Modifiers(0x0001); // MOD_SHIFT
        assert!(
            !c.should_handle_key(&p),
            "Shift+{vk:#x} 出 `{ch}` 且无中文映射，吃了也只能原样吐回，应透传"
        );
    }

    // 不带 Shift 的数字仍旧交还宿主（原有契约不能被上面两条改坏）。
    for vk in 0x30u32..=0x39 {
        assert!(
            !c.should_handle_key(&probe(vk)),
            "{vk:#x} 无 Shift 时空缓冲应交还宿主"
        );
    }
}

/// OEM 标点键同样「会转换的吃、产物原样的透传」（`-`→`VK_INSERT`、`/`→`VK_HELP`、`|`→`VK_F13`，
/// 成因同上）。
///
/// 用同一个键的两个态对照：`-`/`_` 同一个 VK，前者无中文映射该透传、后者转 `——` 必须吃。
///
/// 无映射的那半还要过「按键占用」这一关（引导键 / 方案码元首码）。默认方案若将来把 `-` 配成码元
/// 或引导键，本断言会失败，那是判据正确生效而非回归，改断言即可。
#[test]
fn oem_punct_eaten_only_when_they_convert() {
    let Some(c) = coordinator() else { return };

    // 上挡态有中文映射 ⇒ 必须吃（与占用无关，恒成立）。
    for (vk, ch, cn) in [
        (keymap::VK_MINUS, '_', "——"),
        (keymap::VK_SLASH, '?', "？"),
        (keymap::VK_BACKTICK, '~', "～"),
    ] {
        let mut p = KeyProbe::new(vk);
        p.modifiers = Modifiers(0x0001); // MOD_SHIFT
        assert!(
            c.should_handle_key(&p),
            "Shift+{vk:#x} 出 `{ch}`→`{cn}`，要转换，必须吃"
        );
    }

    // 无 Shift 态有中文映射 ⇒ 同样必须吃。
    for (vk, ch, cn) in [
        (keymap::VK_COMMA, ',', '，'),
        (keymap::VK_PERIOD, '.', '。'),
        (keymap::VK_SEMICOLON, ';', '；'),
        (keymap::VK_BACKSLASH, '\\', '、'),
    ] {
        assert!(
            c.should_handle_key(&probe(vk)),
            "{vk:#x} 出 `{ch}`→`{cn}`，要转换，必须吃"
        );
    }

    // 无中文映射、产物即原样 ⇒ 透传（默认方案未把它们配成引导键或码元）。
    for (vk, ch) in [
        (keymap::VK_MINUS, '-'),
        (keymap::VK_EQUAL, '='),
        (keymap::VK_SLASH, '/'),
    ] {
        assert!(
            !c.should_handle_key(&probe(vk)),
            "{vk:#x} 出 `{ch}` 且无中文映射，吃了也只能原样吐回，应透传"
        );
    }
}

/// 空缓冲下的功能键/数字必须交还宿主。
///
/// 否则空格打不出空格、回车不换行、退格删不掉字、数字打不出来。
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
/// 否则切到英文就打不出字母（核心返回 Consumed 而无文本，宿主两头落空）。
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

/// 吃键判定与实际处理必须一致：凡是判定吃的键，处理后不能既消费又无输出。上面所有用例的总闸门。
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
        seq_char: 0,
    })
}
