use super::split_combo;

/// 软键盘功能键表（`SOFT_FN_KEYS`）里的每个键名，拆完都必须落在 `.app`
/// `KeySynthesizer.keyCodeMap` 认得的那一份上。
///
/// ★ 这条钉的是 0.121 的真实缺陷：功能键点击整条路都通了（帧发得出去、`.app` 收得到），
/// 却因为**键名对不上**在最后一步 `resolveKeyCode` 返回 nil 而静默丢弃。
/// 期望值取自 `KeySynthesizer.swift` 的 `keyCodeMap`——改那张表时这里要跟着改。
#[test]
fn soft_keyboard_fn_keys_land_on_names_the_app_knows() {
    // 与 `wind_ui_types::SOFT_FN_KEYS` 的键名逐条对应。
    let cases = [
        ("backspace", "backspace"),
        ("tab", "tab"),
        ("enter", "enter"),
        ("space", "space"),
        // `.app` 侧只有 `delete`，没有 `del`。
        ("del", "delete"),
        // Windows VK 不能原样透传：`vk:0x14` 会被当成 mac CGKeyCode 20（数字键 2）。
        ("vk:0x14", "capslock"),
    ];
    for (input, want) in cases {
        let (key, mods) = split_combo(input);
        assert_eq!(key, want, "功能键 {input:?} 归一后应是 {want:?}");
        assert!(mods.is_empty(), "功能键不带修饰键");
    }
}

/// 键名表与 `wind_ui_types::SOFT_FN_KEYS` **不能漂移**：那边加了新功能键，
/// 这里的用例集必须跟上，否则新键会重演「静默丢弃」。
#[test]
fn fn_key_case_list_covers_every_soft_key() {
    let covered = ["backspace", "tab", "enter", "space", "del", "vk:0x14"];
    for (name, _) in wind_ui_types::SOFT_FN_KEYS {
        assert!(
            covered.contains(name),
            "SOFT_FN_KEYS 新增了 {name:?}，请在上面那条测试里补一行期望值\
                 （并确认 KeySynthesizer.swift 的 keyCodeMap 认得它）"
        );
    }
}

/// 组合键的修饰键归一：cmd/command → win，control → ctrl，option → alt。
#[test]
fn modifiers_normalize_to_the_app_vocabulary() {
    assert_eq!(
        split_combo("Cmd+Shift+v"),
        ("v".into(), vec!["win".to_string(), "shift".to_string()])
    );
    assert_eq!(
        split_combo("Control+Option+Left"),
        ("left".into(), vec!["ctrl".to_string(), "alt".to_string()])
    );
}

/// 空串不该产出一个「按下空键名」的帧。
#[test]
fn empty_combo_yields_empty_key() {
    assert_eq!(split_combo(""), (String::new(), Vec::new()));
    assert_eq!(split_combo("+"), (String::new(), Vec::new()));
}
