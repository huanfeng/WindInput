//! 守门：软键盘的键位名必须与 `wind-keys` 的键名体系一致。
//!
//! ★ 本仓已经因为「两处键名拼写不一致」吃过亏：触发键配了完全没反应，**不报错、不告警、
//! 静默不匹配**，排查成本极高。软键盘的 [`KEY_ROWS`] 是给用户手写配置用的键名，
//! 一旦与 `wind-keys` 分叉，同样的指纹会立刻重现。
//!
//! 生产代码不依赖 `wind-keys`（那会把 windows / core-graphics 平台依赖拖进一个本该
//! 能在 headless / Android 上编译的纯数据 crate），所以一致性由本测试守。

use wind_keys::keymap::key_name_to_vk;
use wind_softkeyboard::{KEY_ROWS, SLOT_COUNT, all_slots, normalize_slot};

/// 字母与数字键位是 ASCII 直映射，不进 `wind-keys` 的 `KEY_TABLE`；
/// 会漂移的只有符号键名，那正是本测试要盯的。
fn is_ascii_alnum_slot(name: &str) -> bool {
    name.len() == 1
        && name
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

#[test]
fn every_symbol_slot_is_a_known_key_name() {
    for slot in all_slots() {
        if is_ascii_alnum_slot(slot) {
            continue;
        }
        assert!(
            key_name_to_vk(slot).is_some(),
            "键位名 {slot:?} 不在 wind-keys 的键名表里——两套名字已分叉，\
             用户按这个名字写补丁会静默不匹配"
        );
    }
}

#[test]
fn slot_aliases_resolve_to_the_same_key_as_wind_keys() {
    // 别名是给手写配置用的（照着键盘敲 ` 比记住 grave 自然）。它们必须和 wind-keys
    // 指向同一个键，否则「补丁按别名写」与「热键按规范名写」会落到两个键上。
    for (alias, canon) in [
        ("`", "grave"),
        ("-", "minus"),
        ("=", "equal"),
        ("[", "lbracket"),
        ("]", "rbracket"),
        ("\\", "backslash"),
        (";", "semicolon"),
        ("'", "quote"),
        (",", "comma"),
        (".", "period"),
        ("/", "slash"),
    ] {
        assert_eq!(
            normalize_slot(alias),
            Some(canon),
            "软键盘把别名 {alias:?} 归一到了别处"
        );
        assert_eq!(
            key_name_to_vk(alias),
            key_name_to_vk(canon),
            "wind-keys 认为别名 {alias:?} 与 {canon:?} 不是同一个键"
        );
    }
}

#[test]
fn layout_covers_the_ansi_main_block() {
    // 13 + 13 + 11 + 10 = 47。行数或某行键位数写错时，画布会整体错位，
    // 而错位在肉眼看来只是「某些符号跑到了别的键上」，很难归因。
    assert_eq!(KEY_ROWS.len(), 4);
    assert_eq!(KEY_ROWS[0].len(), 13);
    assert_eq!(KEY_ROWS[1].len(), 13);
    assert_eq!(KEY_ROWS[2].len(), 11);
    assert_eq!(KEY_ROWS[3].len(), 10);
    assert_eq!(all_slots().count(), SLOT_COUNT);
}
