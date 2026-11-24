use super::*;

#[test]
fn name_aliases_resolve() {
    assert_eq!(key_name_to_vk("backtick"), Some(VK_BACKTICK));
    assert_eq!(key_name_to_vk("grave"), Some(VK_BACKTICK));
    assert_eq!(key_name_to_vk("`"), Some(VK_BACKTICK));
    // backslash 必须能解析：解析不了时依赖它的特殊模式不触发。
    assert_eq!(key_name_to_vk("backslash"), Some(VK_BACKSLASH));
    assert_eq!(key_name_to_vk("\\"), Some(VK_BACKSLASH));
    assert_eq!(key_name_to_vk("EQUALS"), Some(VK_EQUAL)); // 大小写不敏感
    assert_eq!(key_name_to_vk(" semicolon "), Some(VK_SEMICOLON)); // 去空白
}

#[test]
fn letters_only_with_letter_variant() {
    assert_eq!(key_name_to_vk("z"), None);
    assert_eq!(key_name_to_vk_with_letters("z"), Some(0x5A));
    assert_eq!(key_name_to_vk_with_letters("a"), Some(0x41));
    assert_eq!(key_name_to_vk_with_letters("backslash"), Some(VK_BACKSLASH));
}

/// 两个方向的严格度刻意不同：名字→VK 拒绝字母（配置解析，见
/// `Coordinator::special_trigger_vk`），VK→字符接受字母（呈现，组合区要显示按下的 z）。
#[test]
fn prefix_char_accepts_letters_while_key_name_does_not() {
    assert_eq!(vk_to_prefix_char(VK_Z), None);
    assert_eq!(vk_to_prefix_char_with_letters(VK_Z), Some('z'));
    assert_eq!(vk_to_prefix_char_with_letters(VK_A), Some('a'));
    // 符号仍走 KEY_TABLE，两个版本一致。
    assert_eq!(vk_to_prefix_char_with_letters(VK_BACKTICK), Some('`'));
    assert_eq!(vk_to_prefix_char_with_letters(VK_SEMICOLON), Some(';'));
    // 非字母非符号（如数字键）两个版本都没有映射。
    assert_eq!(vk_to_prefix_char_with_letters(0x31), None);
}

/// 出厂默认折算后的绑定集（`pageupdown` + `minus_equal` / `arrows` + `tab`）。
/// 顺序须与 `Config::migrate_nav_keys_into_session_actions` 的折算序一致：page 先、
/// highlight 后——`classify` 用 `.find()`，顺序即优先级。
fn default_nav_keys() -> NavKeys {
    NavKeys::from_binds([
        ("pageup", NavAction::PagePrev),
        ("pagedown", NavAction::PageNext),
        ("minus", NavAction::PagePrev),
        ("equal", NavAction::PageNext),
        ("up", NavAction::HighlightUp),
        ("down", NavAction::HighlightDown),
        ("shift+tab", NavAction::HighlightUp),
        ("tab", NavAction::HighlightDown),
    ])
}

#[test]
fn nav_classify_config_driven() {
    let nk = default_nav_keys();
    // 专用导航键恒生效
    assert_eq!(
        nk.classify(VK_PRIOR, false, false),
        Some(NavAction::PagePrev)
    );
    assert_eq!(
        nk.classify(VK_NEXT, false, false),
        Some(NavAction::PageNext)
    );
    assert_eq!(
        nk.classify(VK_UP, false, false),
        Some(NavAction::HighlightUp)
    );
    assert_eq!(
        nk.classify(VK_DOWN, false, false),
        Some(NavAction::HighlightDown)
    );
    // tab=下移、shift+tab=上移
    assert_eq!(
        nk.classify(VK_TAB, false, false),
        Some(NavAction::HighlightDown)
    );
    assert_eq!(
        nk.classify(VK_TAB, true, false),
        Some(NavAction::HighlightUp)
    );
    // -/= 仅在 include_printable 时作翻页（码表模式 true，文本模式 false）
    assert_eq!(
        nk.classify(VK_MINUS, false, true),
        Some(NavAction::PagePrev)
    );
    assert_eq!(
        nk.classify(VK_EQUAL, false, true),
        Some(NavAction::PageNext)
    );
    assert_eq!(nk.classify(VK_MINUS, false, false), None);
    assert_eq!(nk.classify(VK_EQUAL, false, false), None);
}

#[test]
fn comma_period_pages() {
    // 设置页提供 comma_period 选项：组名不被识别时会静默无翻页绑定，逗号/句号直接上屏。
    let nk = NavKeys::from_binds([
        ("comma", NavAction::PagePrev),
        ("period", NavAction::PageNext),
    ]);
    assert_eq!(
        nk.classify(VK_COMMA, false, true),
        Some(NavAction::PagePrev)
    );
    assert_eq!(
        nk.classify(VK_PERIOD, false, true),
        Some(NavAction::PageNext)
    );
    // 可打印键：文本/表达式模式（临英/快捷输入）里仍作输入字符，不夺为翻页。
    assert_eq!(nk.classify(VK_COMMA, false, false), None);
    assert_eq!(nk.classify(VK_PERIOD, false, false), None);
}

/// 用户诉求一：Tab 向下翻页、Shift+Tab 向上翻页（把 Tab 从高亮组改到翻页组）。
/// Tab 是功能键（`printable = false`），故在临英 / 快捷输入里同样生效——这与
/// `-`/`=` 那类可打印键刻意不同。
#[test]
fn tab_can_be_rebound_to_paging() {
    let nk = NavKeys::from_binds([
        ("shift+tab", NavAction::PagePrev),
        ("tab", NavAction::PageNext),
    ]);
    assert_eq!(nk.classify(VK_TAB, false, false), Some(NavAction::PageNext));
    assert_eq!(nk.classify(VK_TAB, true, false), Some(NavAction::PagePrev));
}

/// 用户诉求二：CapsLock 向上翻页。解析层必须认得它，且必须被标成「只有 keyup
/// 到得了服务端」——协调器据此把它送进 keyup 分支而非 keydown 链。
#[test]
fn capslock_resolves_as_key_up_only() {
    let k = session_key_name_to_vk("capslock").expect("capslock 应可解析");
    assert_eq!(k.vk, VK_CAPITAL);
    assert!(!k.printable, "CapsLock 不产出字符");
    assert!(
        is_key_up_only_vk(k.vk),
        "CapsLock 只有 keyup 到得了服务端；漏判会让绑定挂在永不触发的 keydown 上"
    );
    // 回归保护：CapsLock 与四个纯修饰键不连号，用区间判定会把它漏掉。
    assert!(!(VK_LSHIFT..=VK_RCONTROL).contains(&VK_CAPITAL));
}

/// `shift+` 前缀只认这一个修饰。带 ctrl/alt 的组合键归 `keys.key_actions`
/// （key_down 热键表）——它们无会话时也要能触发，两张表的语义不同。
#[test]
fn session_key_names_reject_ctrl_alt_combos() {
    assert!(session_key_name_to_vk("ctrl+tab").is_none());
    assert!(session_key_name_to_vk("alt+tab").is_none());
    assert!(session_key_name_to_vk("ctrl+shift+tab").is_none());
    // 拼错的名字返回 None（由调用方在加载期告警），不静默变成别的键。
    assert!(session_key_name_to_vk("pgeup").is_none());
}

/// 符号键复用 `KEY_TABLE`，且必须带 `printable`——丢了这个标志，临英里就打不出减号。
#[test]
fn symbol_session_keys_are_printable() {
    for (name, vk) in [
        ("minus", VK_MINUS),
        ("equal", VK_EQUAL),
        ("lbracket", VK_LBRACKET),
        ("comma", VK_COMMA),
    ] {
        let k = session_key_name_to_vk(name).expect("符号键应可解析");
        assert_eq!(k.vk, vk);
        assert!(
            k.printable,
            "{name} 是可打印键，须在文本模式里回落为输入字符"
        );
    }
}

/// 同一个 (键, shift) 被声明两次时先来的赢（`classify` 用 `.find()`）；
/// 配置折算靠这条实现「page 组优先于 highlight 组」。
#[test]
fn first_bind_wins_for_duplicate_key() {
    let nk = NavKeys::from_binds([
        ("tab", NavAction::PageNext),
        ("tab", NavAction::HighlightDown),
    ]);
    assert_eq!(nk.classify(VK_TAB, false, false), Some(NavAction::PageNext));
}

#[test]
fn prefix_char_roundtrip() {
    assert_eq!(vk_to_prefix_char(VK_BACKTICK), Some('`'));
    assert_eq!(vk_to_prefix_char(VK_SLASH), Some('/'));
    assert_eq!(vk_to_prefix_char(VK_BACKSLASH), Some('\\'));
    assert_eq!(vk_to_prefix_char(0x41), None); // 字母无前缀定义
}

/// 修饰键走独立的解析入口，不进 `KEY_TABLE`：后者是引导键（keydown）的配置解析口，而修饰键
/// 只在 keyup 轻敲上工作，混在一起会让引导键设置项接受一个配得上却永不触发的值。
#[test]
fn modifier_names_resolve_only_via_dedicated_entry() {
    assert_eq!(modifier_name_to_vk("rshift"), Some(VK_RSHIFT));
    assert_eq!(modifier_name_to_vk("RShift"), Some(VK_RSHIFT)); // 大小写不敏感
    assert_eq!(modifier_name_to_vk(" lctrl "), Some(VK_LCONTROL)); // 首尾空白不敏感
    assert_eq!(modifier_name_to_vk("rcontrol"), Some(VK_RCONTROL)); // 别名
    assert_eq!(modifier_name_to_vk("backslash"), None); // 符号键不归它管
    // 反向：引导键的解析口不认修饰键，否则 trigger_keys 里能配 rshift 却永不触发。
    assert_eq!(key_name_to_vk("rshift"), None);
    assert_eq!(key_name_to_vk_with_letters("rshift"), None);
}

/// 纯修饰键判定覆盖四个连号，且不误伤相邻 VK。
#[test]
fn pure_modifier_vk_range_is_exact() {
    for vk in [VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL] {
        assert!(is_pure_modifier_vk(vk), "0x{vk:02X} 应是纯修饰键");
    }
    assert!(!is_pure_modifier_vk(0x9F)); // 区间下界之前
    assert!(!is_pure_modifier_vk(0xA4)); // VK_LMENU，区间上界之后
    assert!(!is_pure_modifier_vk(VK_Z));
}

/// `symbol_keys()` 的键名是跨仓契约：设置页按这些名字匹配下拉选项（`TRIGGER_KEY_OPTIONS`），
/// 对不上就静默表现为「提示永远不出」。
///
/// 快照式断言而非「非空即可」：改 `KEY_TABLE` 某行的首个别名不破坏本仓功能，却会静默切断
/// 那条契约；这里红一下，提醒去同步设置页。
#[test]
fn symbol_key_names_are_a_cross_repo_contract() {
    let names: Vec<&str> = symbol_keys().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        vec![
            "backtick",
            "semicolon",
            "quote",
            "comma",
            "period",
            "slash",
            "lbracket",
            "rbracket",
            "backslash",
            "minus",
            "equal",
        ],
        "改动此列表须同步 wind-setting 的 TRIGGER_KEY_OPTIONS / key_options"
    );
    // 字符侧也一并锁住：core 用它判「该键是不是码元」，错一个就报错冲突。
    let by_name = |n: &str| symbol_keys().find(|(k, _)| *k == n).map(|(_, c)| c);
    assert_eq!(by_name("backtick"), Some('`'));
    assert_eq!(by_name("backslash"), Some('\\'));
    assert_eq!(by_name("semicolon"), Some(';'));
}
