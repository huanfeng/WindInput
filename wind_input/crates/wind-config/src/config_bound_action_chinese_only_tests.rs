use super::*;

/// [`BoundAction::only_in_chinese_mode`] 必须对每一个 A 类动词都表过态。
///
/// 那个方法的 `Action(_)` 载荷是 `String`，编译期穷举不了，新动词会静默落进「两模式都吃」。
/// 预期表按 `DISPATCH_ACTIONS` 逐项列全，新增动词时这里会红。结果同时决定组合键是否带
/// `HOTKEY_POLICY_CHINESE_ONLY` 与分派端是否判 `chinese_mode`。
#[test]
fn chinese_only_covers_every_dispatch_action() {
    // (动词, 是否只在中文态有意义)
    let expected: &[(&str, bool)] = &[
        // 中英/方案切换：恰恰要在非中文态按得动，否则切过去回不来。
        ("toggle_mode", false),
        ("switch_engine", false),
        // 两模式下都该生效。
        ("toggle_full_width", false),
        ("toggle_toolbar", false),
        ("open_settings", false),
        ("open_dictionary", false),
        ("take_screenshot", false),
        // 只在中文态有意义。
        ("toggle_punct", true),
        ("toggle_s2t", true),
        ("toggle_t2s", true),
    ];
    let mut listed: Vec<&str> = expected.iter().map(|(v, _)| *v).collect();
    listed.sort_unstable();
    let mut actual: Vec<&str> = BoundAction::DISPATCH_ACTIONS.to_vec();
    actual.sort_unstable();
    assert_eq!(
        listed, actual,
        "DISPATCH_ACTIONS 变了：请在本表里给新动词表态（只在中文态有意义吗）"
    );
    for (verb, only_chinese) in expected {
        let parsed = BoundAction::parse(verb);
        assert!(
            matches!(parsed, BoundAction::Action(_)),
            "{verb} 应解析成 A 类动作"
        );
        assert_eq!(
            parsed.only_in_chinese_mode(),
            *only_chinese,
            "{verb} 的中文态判据与预期不符"
        );
    }
}

/// 进 overlay 的一族与字词范围恒为真；软键盘与切方案恒为假。
///
/// 软键盘带 `CHINESE_ONLY` 的话英文态打不开，而面板画的「键位 → 符号」与中英态无关
/// （C++ 侧为此专设了 `IsSoftKeyboard()` 总闸）。
#[test]
fn chinese_only_matches_action_family() {
    for verb in [
        "temp_pinyin",
        "temp_english",
        "aux_code",
        "rare_char",
        "reverse",
        "mix:quick_mix",
        "special:fuhao",
        "single_char",
    ] {
        assert!(
            BoundAction::parse(verb).only_in_chinese_mode(),
            "{verb} 只在中文输入中途有意义"
        );
    }
    for verb in [
        "softkeyboard",
        "softkeyboard:num",
        "toggle_schema:english",
        "switch_schema:wubi",
        "comment_state",
        "none",
    ] {
        assert!(
            !BoundAction::parse(verb).only_in_chinese_mode(),
            "{verb} 不得限中文态"
        );
    }
}
