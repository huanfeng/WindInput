use super::*;

#[test]
fn comment_switch_ctrl_registers_key_up_with_session_bit() {
    use crate::comment_lib::{SwitchKey, SwitchMode};
    let up_of = |key: SwitchKey, mode: SwitchMode, toggle: &[&str]| {
        let mut cfg = Config::default();
        cfg.ui.comment.switch_key = key;
        cfg.ui.comment.switch_mode = mode;
        cfg.keys.toggle_mode_keys = toggle.iter().map(|s| s.to_string()).collect();
        cfg.keys.session_actions.clear();
        cfg.keys.select_key_groups.clear();
        Compiler::new(cfg)
            .compile()
            .key_up
            .into_iter()
            .filter(|e| e.action == COMMENT_SWITCH_ACTION)
            .map(|e| e.tsf_hash)
            .collect::<Vec<_>>()
    };
    let l = key_hash(MOD_CTRL | MOD_LCTRL, VK_LCONTROL);
    let r = key_hash(MOD_CTRL | MOD_RCTRL, VK_RCONTROL);
    let s = HOTKEY_POLICY_SESSION;
    assert_eq!(
        up_of(SwitchKey::Ctrl, SwitchMode::Session, &[]),
        [l | s, r | s]
    );
    assert_eq!(up_of(SwitchKey::LCtrl, SwitchMode::Persist, &[]), [l | s]);
    assert_eq!(up_of(SwitchKey::RCtrl, SwitchMode::Hold, &[]), [r | s]);
    assert!(up_of(SwitchKey::Ctrl, SwitchMode::Off, &[]).is_empty());
    assert!(up_of(SwitchKey::Alt, SwitchMode::Session, &[]).is_empty());
    // 已是中英切换键的那一侧不再登记，切换键条目不改它的吃键条件。
    assert_eq!(
        up_of(SwitchKey::Ctrl, SwitchMode::Session, &["lctrl"]),
        [r | s]
    );
}

#[test]
fn test_parse_simple_key() {
    let hash = parse_hotkey("lshift").unwrap();
    assert_eq!(hash, 0xA0); // no modifiers, key=0xA0
}

#[test]
fn test_parse_ctrl_space() {
    let hash = parse_hotkey("Ctrl+Space").unwrap();
    assert_eq!(hash, (MOD_CTRL << 16) | 0x20);
}

#[test]
fn test_parse_ctrl_shift_e() {
    let hash = parse_hotkey("ctrl+shift+e").unwrap();
    assert_eq!(hash, ((MOD_CTRL | MOD_SHIFT) << 16) | 0x45);
}

#[test]
fn test_parse_shift_space() {
    let hash = parse_hotkey("shift+space").unwrap();
    assert_eq!(hash, (MOD_SHIFT << 16) | 0x20);
}

#[test]
fn test_parse_ctrl_dot() {
    let hash = parse_hotkey("ctrl+.").unwrap();
    assert_eq!(hash, (MOD_CTRL << 16) | 0xBE);
}

#[test]
fn test_parse_ctrl_equal() {
    let hash = parse_hotkey("ctrl+equal").unwrap();
    assert_eq!(hash, (MOD_CTRL << 16) | 0xBB);
}

#[test]
fn test_parse_empty() {
    assert!(parse_hotkey("").is_none());
}

#[test]
fn test_toggle_mode_key_includes_specific_modifier() {
    // 关键回归：lshift 的 keyUp hash 必须同时含通用位(MOD_SHIFT)和具体位(MOD_LSHIFT)，
    // 否则 C++ TSF 算出的 0x1100A0 在白名单里找不到 → Shift 切换失效。
    let lshift = compile_toggle_mode_key("lshift").unwrap();
    assert_eq!(lshift, key_hash(MOD_SHIFT | MOD_LSHIFT, VK_LSHIFT));
    assert_eq!(lshift, 0x0011_00A0);

    let rshift = compile_toggle_mode_key("rshift").unwrap();
    assert_eq!(rshift, key_hash(MOD_SHIFT | MOD_RSHIFT, VK_RSHIFT));
    assert_eq!(rshift, 0x0021_00A1);
}

#[test]
fn test_number_hotkey_expands_to_ten_session_keys() {
    let entries = compile_number_hotkey("ctrl+shift+number");
    assert_eq!(entries.len(), 10);
    // tsf_hash 含 session policy 位；match_hash 为 raw
    assert!(entries[0].tsf_hash & HOTKEY_POLICY_SESSION != 0);
    assert_eq!(entries[0].match_hash, key_hash(MOD_CTRL | MOD_SHIFT, 0x30));
    assert!(compile_number_hotkey("none").is_empty());
}

#[test]
fn test_number_template_mods_is_whitelist_not_parser() {
    // 白名单，不是「解析得动就收」：值域外的写法一律不绑定（含已删除的
    // `parse_hotkey_prefix` 曾经会放行的 alt/win 变体）。
    assert_eq!(number_template_mods("ctrl+number"), Some(MOD_CTRL));
    assert_eq!(
        number_template_mods("Ctrl+Shift+Number"), // 大小写/空白不敏感
        Some(MOD_CTRL | MOD_SHIFT)
    );
    assert_eq!(number_template_mods(" ctrl+number "), Some(MOD_CTRL));
    assert_eq!(
        number_template_mods("ctrl+alt+number"),
        Some(MOD_CTRL | MOD_ALT)
    );
    for bad in ["none", "", "alt+number", "win+number", "ctrl+shift+e"] {
        assert_eq!(number_template_mods(bad), None, "{bad:?} 不该被接受");
    }
}

#[test]
fn test_number_template_mods_are_pairwise_distinct() {
    // 回归：消费端按相等比较修饰位，前提是任意两项的位集合不相等。
    // （更强的性质——不存在子集关系——已不再要求：等值判据对子集免疫，
    //   这正是 `ctrl+alt+number` 能安全加进值域的原因。）
    let all = ["ctrl+number", "ctrl+shift+number", "ctrl+alt+number"];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(
                number_template_mods(a),
                number_template_mods(b),
                "{a:?} 与 {b:?} 修饰位相同，等值判据无法区分"
            );
        }
    }
}

#[test]
fn test_compile_switch_engine_match_hash() {
    let mut cfg = Config::default();
    cfg.keys.switch_engine = "ctrl+shift+e".to_string();
    cfg.keys.toggle_mode_keys = vec!["lshift".into(), "rshift".into()];
    let compiled = Compiler::new(cfg).compile();
    // switch_engine 无 policy 位，match_hash == tsf_hash == 0x30045
    let se = compiled
        .key_down
        .iter()
        .find(|e| e.action == "switch_engine")
        .unwrap();
    assert_eq!(se.match_hash, key_hash(MOD_CTRL | MOD_SHIFT, 0x45));
    assert_eq!(se.tsf_hash, se.match_hash);
    // 规范化匹配：带 L/R 具体位的入站事件也能命中
    assert_eq!(
        compiled.match_key_down(key_hash(MOD_CTRL | MOD_SHIFT, 0x45)),
        Some("switch_engine")
    );
    // 出厂的注释切换键（Ctrl）另有两条，不计。
    assert_eq!(
        compiled
            .key_up
            .iter()
            .filter(|e| e.action == "toggle_mode")
            .count(),
        2
    );
}

#[test]
fn open_add_word_dialog_registered_chinese_only() {
    let mut cfg = Config::default();
    cfg.keys.open_add_word_dialog = "ctrl+shift+equal".to_string();
    let compiled = Compiler::new(cfg).compile();
    // action 串应出现在 key_down 组
    assert!(
        compiled
            .key_down
            .iter()
            .any(|e| e.action == "open_add_word_dialog"),
        "open_add_word_dialog 应注册进 key_down"
    );
}

/// 把 (键名, 动词) 对拼成 `keys.session_actions`。测试直接构造 `Compiler`、不经
/// `Config::normalize`，故须显式写出折算后的形态。
fn session_actions(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// 清空四组键组配置，使 `effective_session_actions()` 只剩显式表。出厂这几组都非空，
/// 不清的话每个用例都会多出折算来的键。
fn only_explicit(cfg: &mut Config) {
    cfg.keys.page_keys.clear();
    cfg.keys.highlight_keys.clear();
    cfg.keys.select_key_groups.clear();
    cfg.keys.select_char_keys.clear();
}

/// 四组键组配置必须经 `effective_session_actions()` 进 TSF 转发表。
///
/// 折算在消费层视图里做；编译器若直接读 `config.keys.session_actions`（只有用户显式配的
/// 部分），出厂翻页/选词键一个都进不了转发表，而配置文件看着好好的。
#[test]
fn key_group_config_reaches_forward_set_without_explicit_table() {
    let cfg = Config::default();
    assert!(
        cfg.keys.session_actions.is_empty(),
        "前置条件：出厂默认不该有显式会话态绑定，否则本测试证明不了什么"
    );
    let compiled = Compiler::new(cfg).compile();
    // 默认 page_keys 含 pageupdown、highlight_keys 含 arrows、select_key_groups 含 semicolon_quote。
    for (vk, label) in [
        (VK_PRIOR, "PageUp（page_keys 默认）"),
        (VK_NEXT, "PageDown（page_keys 默认）"),
        (VK_UP, "↑（highlight_keys 默认）"),
        (VK_OEM_1, "分号（select_key_groups 默认）"),
    ] {
        assert!(
            compiled.key_down.iter().any(
                |e| e.match_hash & 0xFFFF == vk && e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0
            ),
            "{label} 未进 FORWARD_ONLY 转发集——四组键组配置没被编译进去"
        );
    }
}

#[test]
fn ctrl_arrow_forward_registration_can_be_disabled() {
    let mut cfg = Config::default();
    let has = |c: &Config, vk: u32| {
        Compiler::new(c.clone())
            .compile()
            .key_down
            .iter()
            .any(|e| e.match_hash == key_hash(MOD_CTRL, vk))
    };
    assert!(
        has(&cfg, VK_LEFT) && has(&cfg, VK_RIGHT),
        "出厂转发 Ctrl+←/→"
    );
    cfg.keys.session_actions = session_actions(&[("ctrl+left", "none")]);
    assert!(!has(&cfg, VK_LEFT), "写 none 后 Ctrl+← 不再转发");
    assert!(has(&cfg, VK_RIGHT), "另一个方向不受影响");
}

#[test]
fn forward_only_bit_marks_page_and_select_keys_only() {
    let mut cfg = Config::default();
    only_explicit(&mut cfg);
    cfg.keys.toggle_full_width = "shift+space".to_string();
    // 等价于旧的 `page_keys = ["minus_equal", "shift_tab"]` +
    // `select_key_groups = ["semicolon_quote"]` 折算后的样子。
    cfg.keys.session_actions = session_actions(&[
        ("minus", "page_prev"),
        ("equal", "page_next"),
        ("shift+tab", "page_prev"),
        ("tab", "page_next"),
        ("semicolon", "select_candidate:2"),
        ("quote", "select_candidate:3"),
    ]);
    let compiled = Compiler::new(cfg).compile();

    // 判据不能用「action 为空」：pin/delete 候选的数字热键 action 同样是空串
    //（动作由服务端按 hash 自认），它们是 session 热键、不该带 FORWARD_ONLY。
    // 只有会话态键才是仅注册转发，故按 raw hash 精确点名。
    let forward_only_raw: Vec<u32> = vec![
        key_hash(0, VK_OEM_MINUS),
        key_hash(0, VK_OEM_PLUS),
        key_hash(MOD_SHIFT, VK_TAB),
        key_hash(0, VK_TAB),
        key_hash(0, VK_OEM_1),
        key_hash(0, VK_OEM_7),
        // 出厂注释切换键 F1 补进会话按键表，同样只是转发登记。
        key_hash(0, 0x70),
        // Ctrl+←/→ 整音节跳：会话键表不带 Ctrl，由编译器单独登记转发。
        key_hash(MOD_CTRL, VK_LEFT),
        key_hash(MOD_CTRL, VK_RIGHT),
    ];
    assert_eq!(
        forward_only_raw.len(),
        9,
        "样例绑定应展开出 6 个键，另加 F1 与 Ctrl+←/→"
    );

    for e in &compiled.key_down {
        let expected = forward_only_raw.contains(&e.match_hash);
        assert_eq!(
            e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0,
            expected,
            "hash=0x{:08X} action={:?} 的 FORWARD_ONLY 位不符预期",
            e.tsf_hash,
            e.action
        );
        // match_hash 是服务端匹配用的裸 hash，任何 policy 位都不该混进去
        assert_eq!(e.match_hash & HOTKEY_POLICY_FORWARD_ONLY, 0);
    }

    // 定桩：shift+space 无任何 policy 位，TSF 侧必须无条件吃。
    let fw = compiled
        .key_down
        .iter()
        .find(|e| e.action == "toggle_full_width")
        .expect("toggle_full_width 应在 key_down 组");
    assert_eq!(fw.tsf_hash, fw.match_hash);
    assert_eq!(fw.tsf_hash, key_hash(MOD_SHIFT, 0x20));
}

/// 修饰键作选词键时只进 key_up，绝不进 key_down。
///
/// TSF 的 keydown 查表用「通用修饰位 + 笼统 VK_CONTROL」，对不上这里登记的具体 VK；
/// 何况纯修饰键 keydown 必须放行。action 用统一的 `session_action` 是安全的：keyup 选词
/// 只按 VK 查偏移，唯一读 action 的 `is_toggle_mode_keycode` 只认 `toggle_mode`。
#[test]
fn select_modifier_group_registers_on_key_up_only() {
    let mut cfg = Config::default();
    cfg.keys.toggle_mode_keys = vec!["lshift".into(), "rshift".into()];
    // 等价于旧的 `select_key_groups = ["lrctrl"]` 折算后的样子。
    cfg.keys.session_actions = session_actions(&[
        ("lctrl", "select_candidate:2"),
        ("rctrl", "select_candidate:3"),
    ]);
    let compiled = Compiler::new(cfg).compile();

    assert!(
        !compiled
            .key_down
            .iter()
            .any(|e| matches!(e.match_hash & 0xFFFF, VK_LCONTROL | VK_RCONTROL)),
        "修饰键选词键不得出现在 key_down"
    );
    let sel: Vec<&HotkeyEntry> = compiled
        .key_up
        .iter()
        .filter(|e| e.action == SESSION_ACTION)
        .collect();
    assert_eq!(sel.len(), 2, "lrctrl 应展开出左右两个 keyup 登记");
    assert_eq!(
        sel[0].match_hash,
        key_hash(MOD_CTRL | MOD_LCTRL, VK_LCONTROL)
    );
    assert_eq!(
        sel[1].match_hash,
        key_hash(MOD_CTRL | MOD_RCTRL, VK_RCONTROL)
    );
    // 与 toggle 登记同格式（通用位+具体位），否则 C++ GetCurrentModifiers 的双位哈希对不上。
    assert_eq!(
        compiled
            .key_up
            .iter()
            .filter(|e| e.action == "toggle_mode")
            .count(),
        2,
        "切换键登记不应被选词登记挤掉"
    );
}

/// `capslock = "page_prev"`：CapsLock 只有 keyup 到得了服务端，故必须进 `key_up` 表并带
/// SESSION 位——C++ 靠这个位区分「toggle 语义」（恒吃 keydown）与「会话语义」。
///
/// action 不能是 `toggle_mode`：`is_toggle_mode_keycode` 按 action 过滤，混用会让空闲时
/// 敲 CapsLock 莫名切中英文。
#[test]
fn capslock_session_action_registers_on_key_up_with_session_policy() {
    let mut cfg = Config::default();
    cfg.keys.session_actions = session_actions(&[("capslock", "page_prev")]);
    let compiled = Compiler::new(cfg).compile();

    let e = compiled
        .key_up
        .iter()
        .find(|e| e.action == SESSION_ACTION)
        .expect("capslock 的会话态绑定应登记进 key_up");
    assert_eq!(e.match_hash, key_hash(MOD_CAPSLOCK, VK_CAPITAL));
    assert!(
        e.tsf_hash & HOTKEY_POLICY_SESSION != 0,
        "缺 SESSION 位，C++ 会把它当 toggle 键处理，keydown 恒被吃 ⇒ 大小写切换全局失效"
    );
    assert_eq!(
        e.match_hash & HOTKEY_POLICY_SESSION,
        0,
        "match_hash 是服务端匹配用的裸 hash，policy 位不该混进去"
    );
    assert_ne!(
        e.action, "toggle_mode",
        "见本测试文档：会让空闲敲 CapsLock 切中英文"
    );
    assert!(
        !compiled
            .key_down
            .iter()
            .any(|e| e.match_hash & 0xFFFF == VK_CAPITAL),
        "CapsLock 不得进 key_down —— C++ 侧根本不发它的 keydown"
    );
}

/// 会话态的功能键走 key_down + FORWARD_ONLY（与旧 `page_keys` 的登记形态一致）。
/// 无会话时 C++ 靠 FORWARD_ONLY 闸门放行，键回落宿主的原语义（Tab 仍是制表符）。
#[test]
fn session_function_keys_register_on_key_down_forward_only() {
    let mut cfg = Config::default();
    cfg.keys.session_actions = session_actions(&[("tab", "page_next"), ("shift+tab", "page_prev")]);
    let compiled = Compiler::new(cfg).compile();

    for (want_mods, label) in [(0, "tab"), (MOD_SHIFT, "shift+tab")] {
        let raw = key_hash(want_mods, VK_TAB);
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.match_hash == raw)
            .unwrap_or_else(|| panic!("{label} 应登记进 key_down"));
        assert!(
            e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0,
            "{label} 缺 FORWARD_ONLY：无会话时 TSF 会把它当动作热键吞掉"
        );
        assert!(
            e.action.is_empty(),
            "会话态键是仅注册转发，动作由服务端自认"
        );
    }
}

/// 显式 `none` 与拼错的键名都不产出登记，且不 panic。
#[test]
fn session_actions_skip_disabled_and_unknown_keys() {
    let mut cfg = Config::default();
    only_explicit(&mut cfg);
    cfg.keys.session_actions =
        session_actions(&[("tab", "none"), ("pgeup", "page_prev"), ("up", "")]);
    let compiled = Compiler::new(cfg).compile();
    assert!(
        !compiled
            .key_down
            .iter()
            .any(|e| matches!(e.match_hash & 0xFFFF, VK_TAB | VK_UP)),
        "none / 空值不应登记"
    );
}

/// 可打印选词键的通路不变：仍在 key_down 且带 FORWARD_ONLY，不进 key_up。
#[test]
fn printable_select_group_stays_on_key_down() {
    let mut cfg = Config::default();
    cfg.keys.session_actions = session_actions(&[
        ("semicolon", "select_candidate:2"),
        ("quote", "select_candidate:3"),
    ]);
    let compiled = Compiler::new(cfg).compile();
    for raw in [key_hash(0, VK_OEM_1), key_hash(0, VK_OEM_7)] {
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.match_hash == raw)
            .expect("可打印选词键应在 key_down");
        assert!(e.tsf_hash & HOTKEY_POLICY_FORWARD_ONLY != 0);
    }
    assert!(
        !compiled
            .key_up
            .iter()
            .any(|e| e.action == "select_candidate"),
        "可打印选词键不该跑到 key_up 去"
    );
}

/// 软键盘开关键住在专用字段，策略位与它此前在 `key_actions` 里那条逐位相同：
/// 带 GLOBAL、不带 CHINESE_ONLY——面板画的是键位→符号的映射，与中英文态无关，
/// 带上那个位英文态就连开都开不出来。
#[test]
fn softkeyboard_field_carries_global_but_not_chinese_only() {
    let cfg = Config::default();
    assert_eq!(
        cfg.keys.softkeyboard, "ctrl+shift+k",
        "出厂默认应绑 Ctrl+Shift+K"
    );
    let compiled = Compiler::new(cfg).compile();
    let e = compiled
        .key_down
        .iter()
        .find(|e| e.action == "softkeyboard")
        .expect("软键盘开关键应进 key_down 表");
    assert!(e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0, "应带 GLOBAL 位");
    assert!(
        e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY == 0,
        "不得带 CHINESE_ONLY——带上英文态连开都开不出来"
    );
}

/// 核心不变量：出厂绑定关得掉。
///
/// 出厂值若写在 `keys.key_actions` 里，那张表四层深合并、只能新增/覆盖，用户删掉后每次
/// `load()` 都被灌回来；放在标量字段里，清空就是清空。
#[test]
fn softkeyboard_field_cleared_disables_the_hotkey() {
    let mut cfg = Config::default();
    cfg.keys.softkeyboard = String::new();
    let compiled = Compiler::new(cfg).compile();
    assert!(
        !compiled.key_down.iter().any(|e| e.action == "softkeyboard"),
        "清空后不得再注册软键盘热键"
    );
}

/// `key_actions` 里的显式 `none` 压制更低层（L2 出厂 / L2.5 定制版）在同一张表里写的绑定。
/// `merge_value` 表达不了删除，禁用只能靠显式值，语义与单键、修饰键两条通路的
/// `BoundAction::None` 对应。第二条断言防「整个循环被跳过」也让第一条绿。
#[test]
fn explicit_none_keeps_a_combo_binding_out_of_the_table() {
    let mut cfg = Config::default();
    // 专用字段清空，免得出厂那条 softkeyboard 占着同一个键干扰判定。
    cfg.keys.softkeyboard = String::new();
    cfg.keys.key_actions = [
        ("ctrl+shift+k".to_string(), "none".to_string()),
        ("ctrl+shift+u".to_string(), "special:fuhao".to_string()),
    ]
    .into_iter()
    .collect();
    let compiled = Compiler::new(cfg).compile();

    let raw_k = parse_hotkey("ctrl+shift+k").expect("ctrl+shift+k 应解析得动");
    assert!(
        !compiled.key_down.iter().any(|e| e.match_hash == raw_k),
        "显式 none 的条目不得进热键表"
    );
    assert!(
        compiled
            .key_down
            .iter()
            .any(|e| e.action == "special:fuhao"),
        "同表里其它条目不受影响"
    );
}

#[test]
fn add_word_hotkeys_carry_global_policy() {
    let mut cfg = Config::default();
    cfg.keys.add_word = "ctrl+equal".to_string();
    cfg.keys.open_add_word_dialog = "ctrl+shift+equal".to_string();
    cfg.keys.toggle_punct = "ctrl+period".to_string();
    let compiled = Compiler::new(cfg).compile();

    let find = |a: &str| {
        compiled
            .key_down
            .iter()
            .find(|e| e.action == a)
            .unwrap()
            .clone()
    };

    // 加词两键：CHINESE_ONLY + GLOBAL 叠加
    for a in ["add_word", "open_add_word_dialog"] {
        let e = find(a);
        assert!(
            e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0,
            "{a} 的 tsf_hash 应带 GLOBAL 位"
        );
        assert!(
            e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
            "{a} 的 tsf_hash 应仍带 CHINESE_ONLY 位"
        );
        // match_hash 是规范化的原始 hash，不含任何 policy 位
        assert_eq!(
            e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
            0,
            "{a} 的 match_hash 不应含 policy 位"
        );
    }

    // 其它 chinese-only 键（toggle_punct）不该被全局拦截，避免多抢宿主快捷键
    let tp = find("toggle_punct");
    assert!(
        tp.tsf_hash & HOTKEY_POLICY_GLOBAL == 0,
        "toggle_punct 不应带 GLOBAL 位"
    );
    assert!(tp.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0);
}

/// 裸 F1–F12 绑的动作只在中文模式吃键（宿主的 F1 帮助 / F5 刷新英文态归宿主）；
/// 「离开 / 返回英文态」的动词例外；带修饰键的 F 组合键不受影响。
#[test]
fn bare_function_key_is_chinese_only_except_mode_switch_verbs() {
    let compile = |key: &str, verb: &str| {
        let mut cfg = Config::default();
        cfg.keys
            .key_actions
            .insert(key.to_string(), verb.to_string());
        Compiler::new(cfg)
            .compile()
            .key_down
            .into_iter()
            .find(|e| e.match_hash == parse_hotkey(key).expect("键名合法"))
            .expect("应进表")
            .tsf_hash
    };
    assert!(compile("f5", "command:ime.reload()") & HOTKEY_POLICY_CHINESE_ONLY != 0);
    assert!(compile("f5", "comment_state:detail") & HOTKEY_POLICY_CHINESE_ONLY != 0);
    assert_eq!(compile("f6", "toggle_mode") & HOTKEY_POLICY_CHINESE_ONLY, 0);
    assert_eq!(
        compile("f6", "toggle_schema:wubi86") & HOTKEY_POLICY_CHINESE_ONLY,
        0
    );
    assert_eq!(
        compile("ctrl+f5", "command:ime.reload()") & HOTKEY_POLICY_CHINESE_ONLY,
        0,
        "带修饰键的 F 组合键仍按动词的老规矩"
    );
}

/// 特殊模式直达热键写在 `keys.key_actions` 里（`special:<方案id>`），动词原样
/// 进表（分派端用 `BoundAction::parse` 认串），并带 CHINESE_ONLY | GLOBAL。
#[test]
fn special_mode_hotkey_compiles_with_global_policy() {
    let mut cfg = Config::default();
    cfg.keys
        .key_actions
        .insert("ctrl+shift+u".to_string(), "special:rare".to_string());
    let compiled = Compiler::new(cfg).compile();
    let e = compiled
        .key_down
        .iter()
        .find(|e| e.action == "special:rare")
        .expect("key_actions 的 special:<id> 应原样进表");
    // 与加词键同策略：CHINESE_ONLY | GLOBAL；match_hash 不含任何 policy 位
    assert!(e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0);
    assert!(e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0);
    assert_eq!(
        e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
        0
    );
}

#[test]
fn temp_pinyin_hotkey_compiles_with_global_policy() {
    let mut cfg = Config::default();
    cfg.input.temp_pinyin.hotkey = "ctrl+shift+p".to_string();
    let compiled = Compiler::new(cfg).compile();
    let e = compiled
        .key_down
        .iter()
        .find(|e| e.action == "temp_pinyin")
        .expect("temp_pinyin.hotkey 应编出动词原文 temp_pinyin");
    assert!(e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0);
    assert!(e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0);
    assert_eq!(
        e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
        0
    );
}

/// `keys.key_actions` 编出对应动词，且与方案直达热键同策略（不带 CHINESE_ONLY）。
/// 带上它，切到英文方案后回程那一下就不响应了，功能恰好废掉一半。
#[test]
fn key_actions_compile_toggle_schema_without_chinese_only_policy() {
    let mut cfg = Config::default();
    cfg.keys.key_actions.insert(
        "ctrl+shift+n".to_string(),
        "toggle_schema:english".to_string(),
    );
    let compiled = Compiler::new(cfg).compile();
    let e = compiled
        .key_down
        .iter()
        .find(|e| e.action == "toggle_schema:english")
        .expect("keys.key_actions 应编出 toggle_schema:<id>");
    assert_eq!(
        e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY,
        0,
        "往返热键不得带 CHINESE_ONLY，否则从英文方案回不来"
    );
}

/// 不支持的动词与解析不了的键都被丢弃，不进热键表；混进表里的话按下时分发端匹配不上，
/// 与热键没注册上同形，用户无从分辨。
#[test]
fn key_actions_drop_unknown_verbs_and_unparsable_keys() {
    let mut cfg = Config::default();
    cfg.keys.key_actions.insert(
        "ctrl+shift+n".to_string(),
        "no_such_verb:english".to_string(),
    );
    cfg.keys
        .key_actions
        .insert("ctrl+shift+m".to_string(), "toggle_schema:".to_string());
    cfg.keys.key_actions.insert(
        "这不是热键".to_string(),
        "toggle_schema:english".to_string(),
    );
    let n = Config::default();
    let base = Compiler::new(n).compile().key_down.len();
    let compiled = Compiler::new(cfg).compile();
    assert_eq!(compiled.key_down.len(), base, "三条非法项都不该进热键表");
}

/// `switch_schema:<id>`（单向）能编进 key_down 表，且不带 CHINESE_ONLY。
///
/// 带上它，切到英文方案后热键就不再响应，切得过去切不回来。特殊模式热键需要这个位
/// （overlay 只在中文输入中途有意义），方案切换恰恰相反。
#[test]
fn switch_schema_compiles_without_chinese_only_policy() {
    let mut cfg = Config::default();
    cfg.keys.key_actions.insert(
        "ctrl+shift+r".to_string(),
        "switch_schema:wubi86".to_string(),
    );
    let compiled = Compiler::new(cfg).compile();
    let e = compiled
        .key_down
        .iter()
        .find(|e| e.action == "switch_schema:wubi86")
        .expect("switch_schema 应编进 key_down 表");
    assert_eq!(
        e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY,
        0,
        "方案切换热键不得带 CHINESE_ONLY，否则英文方案下切不回中文方案"
    );
    // 反向对照：同一份编译产物里，特殊模式热键确实是带 CHINESE_ONLY 的——
    // 否则「不带」这条断言在 policy 位整体失效时也会通过。
    let mut cfg2 = Config::default();
    cfg2.keys
        .key_actions
        .insert("ctrl+shift+u".to_string(), "special:rare".to_string());
    let c2 = Compiler::new(cfg2).compile();
    let e2 = c2
        .key_down
        .iter()
        .find(|e| e.action == "special:rare")
        .unwrap();
    assert!(
        e2.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
        "对照组：特殊模式热键应带 CHINESE_ONLY"
    );
}

/// 残留的 `keys.schema_hotkeys` 不再生效，且不悄悄折算进 key_actions。
///
/// 兼容层已删除，但必须可见地失效：`normalize` 发告警后清空该表，留着会让「有没有配过」
/// 的判断读到假信号。告警文本不断言（文案会改）。
#[test]
fn legacy_schema_hotkeys_are_dropped_not_migrated() {
    let mut cfg = Config::default();
    cfg.keys
        .legacy_schema_hotkeys
        .insert("wubi86".to_string(), "ctrl+shift+r".to_string());
    cfg.normalize();
    assert!(
        cfg.keys.legacy_schema_hotkeys.is_empty(),
        "告警后须清空，否则后续「有没有配过」的判断会读到假信号"
    );
    assert!(
        !cfg.keys.key_actions.contains_key("ctrl+shift+r"),
        "兼容层已删除，不该再折算进 key_actions"
    );
    let compiled = Compiler::new(cfg).compile();
    assert!(
        !compiled
            .key_down
            .iter()
            .any(|e| e.action.starts_with("switch_schema:")),
        "残留旧键不得产生任何热键条目"
    );
}

/// 动词 id 为空（`special:`）不产生条目；`temp_pinyin.hotkey` 默认空同理。
/// 身份收敛到方案 id 后它不可能为空，这里只防脏数据。
#[test]
fn empty_or_idless_mode_hotkey_produces_no_entry() {
    let mut cfg = Config::default();
    cfg.keys
        .key_actions
        .insert("ctrl+shift+u".to_string(), "special:".to_string());
    cfg.keys
        .key_actions
        .insert("ctrl+shift+i".to_string(), "special:   ".to_string());
    let compiled = Compiler::new(cfg).compile();
    assert!(
        !compiled
            .key_down
            .iter()
            .any(|e| e.action.starts_with("special:") || e.action == "temp_pinyin"),
        "空 hotkey / 空 id 不应产生直达热键条目"
    );
}

/// 修饰键的 keyup hash 必须带通用位 + 具体位：C++ `GetCurrentModifiers()` 对
/// 修饰键同时返回两者，只带一边会匹配不上（表现为「绑了没反应」）。
/// 与 `compile_toggle_mode_key` 同格式——两者服务于同一条 TSF keyup 通路。
#[test]
fn modifier_key_up_hash_matches_toggle_format() {
    assert_eq!(
        compile_modifier_key_up_hash(VK_RSHIFT),
        compile_toggle_mode_key("rshift"),
        "同一个键经两条入口应得到同一个 hash"
    );
    assert_eq!(
        compile_modifier_key_up_hash(VK_LCONTROL),
        compile_toggle_mode_key("lctrl")
    );
    // 低 16 位是 VK，供 is_pure_modifier_vk / 分派点反查。
    let h = compile_modifier_key_up_hash(VK_RSHIFT).unwrap();
    assert_eq!(h & 0xFFFF, VK_RSHIFT);
    // 非修饰键没有 keyup 形态（CapsLock 也不在此列：它走 toggle_mode_keys 那条）。
    assert_eq!(compile_modifier_key_up_hash(VK_OEM_1), None);
    assert_eq!(compile_modifier_key_up_hash(VK_CAPITAL), None);
}

/// `keys.key_actions` 按键形态分三条通路，不按动词。
#[test]
fn key_action_routes_split_by_key_shape() {
    use KeyActionRoute::*;
    assert_eq!(route_of_key_action("ctrl+shift+n"), Some(Hotkey));
    assert_eq!(route_of_key_action("ctrl+space"), Some(Hotkey));
    assert_eq!(route_of_key_action("rshift"), Some(ModifierKeyUp));
    assert_eq!(route_of_key_action("lctrl"), Some(ModifierKeyUp));
    assert_eq!(route_of_key_action("backtick"), Some(LeadingKey));
    assert_eq!(route_of_key_action("semicolon"), Some(LeadingKey));
    assert_eq!(route_of_key_action("z"), Some(LeadingKey));
    assert_eq!(route_of_key_action("f1"), Some(Hotkey));
    assert_eq!(route_of_key_action("F12"), Some(Hotkey));
    assert_eq!(route_of_key_action("不存在的键"), None);
}

/// 单个有字符的键绝不能进 key_down 热键表。
///
/// `parse_hotkey("backtick")` 返回无修饰位的裸 VK（0xC0），进表后 TSF 会当热键吞掉，
/// `` ` `` 在所有方案里都再也打不出来。
#[test]
fn single_character_key_never_enters_keydown_hotkeys() {
    let mut cfg = Config::default();
    cfg.keys
        .key_actions
        .insert("backtick".into(), "temp_pinyin".into());
    cfg.keys
        .key_actions
        .insert("semicolon".into(), "mix:quick_mix".into());
    cfg.keys
        .key_actions
        .insert("rshift".into(), "toggle_mode".into());
    cfg.keys
        .key_actions
        .insert("ctrl+shift+n".into(), "toggle_schema:wubi86".into());
    // 折算后的选词绑定（本测试直接构造 Compiler、不经 normalize，故显式写出）。
    // 顺带覆盖一个真实场景：同一个键 `;` 同时在两张表里——无会话时是 mix 引导键，
    // 有会话时是次选键。两张表按触发态分野，本就该能共存。
    cfg.keys.session_actions = session_actions(&[
        ("semicolon", "select_candidate:2"),
        ("quote", "select_candidate:3"),
    ]);
    let compiled = Compiler::new(cfg).compile();

    // 判据是「有没有产生带这个动词的 key_down 条目」，不是「这个 VK 在不在
    // key_down 里」——`;` / `'` 同时被会话态表以 FORWARD_ONLY 登记着，按 VK 判会把
    // 那条误当成本段的产物，测了个寂寞。
    for verb in ["temp_pinyin", "mix:quick_mix"] {
        assert!(
            !compiled.key_down.iter().any(|e| e.action == verb),
            "单键条目 {verb} 不该产生 key_down 热键，实际 {:?}",
            compiled
                .key_down
                .iter()
                .map(|e| &e.action)
                .collect::<Vec<_>>()
        );
    }
    // 反向确认分流真的生效：会话态那条 `;` 仍在（action 为空的转发登记），
    // 说明上面的「没有」不是因为整段编译被跳过了。
    assert!(
        compiled
            .key_down
            .iter()
            .any(|e| (e.match_hash & 0xFFFF) == VK_OEM_1 && e.action.is_empty()),
        "会话态表里 `;` 的转发登记应不受影响"
    );

    // 组合键照常进 key_down（收编不该动这条既有通路）。
    assert!(
        compiled
            .key_down
            .iter()
            .any(|e| e.action == "toggle_schema:wubi86"),
        "组合键条目应仍走热键通路"
    );
    // 修饰键进 key_up，且 action 是 schema_bound 而非动词本身——
    // `is_toggle_mode_keycode` 按 action 过滤，塞动词进去它就认不出来了。
    let up = compiled
        .key_up
        .iter()
        .find(|e| (e.match_hash & 0xFFFF) == VK_RSHIFT)
        .expect("rshift 应进 key_up 转发集");
    assert_eq!(up.action, "schema_bound");
}

/// 全局拦截热键的槽位上限护栏必须真的能触发，不能是一条恒假的不等式。
///
/// 两头都验：出厂配置下不报（不是噪音），配满 overlay 组合键时报得出来、且点得出是哪几个。
#[test]
fn global_hotkey_slot_guard_can_actually_fire() {
    // ① 出厂基线不该触发——否则它就是常驻噪音，很快会被无视。
    let base = Compiler::new(Config::default()).compile();
    let (base_total, base_dropped) = global_hotkey_overflow(&base.key_down);
    assert!(
        base_dropped.is_empty(),
        "出厂配置不该触发槽位告警（当前 {base_total} 个带 GLOBAL）"
    );
    assert!(
        base_total <= GLOBAL_HOTKEY_SLOTS,
        "出厂基线本身就顶满槽位了，护栏的余量判据要重算"
    );

    // ② 配满进 overlay 的组合键 ⇒ 必须报，且点得出是哪几个。
    //    用 `rare_char`（无载荷、恒带 CHINESE_ONLY|GLOBAL）逐键绑满。
    let mut cfg = Config::default();
    for (i, vk) in ('a'..='z').enumerate().take(GLOBAL_HOTKEY_SLOTS + 4) {
        let _ = i;
        cfg.keys
            .key_actions
            .insert(format!("ctrl+alt+shift+{vk}"), "rare_char".to_string());
    }
    let full = Compiler::new(cfg).compile();
    let (total, dropped) = global_hotkey_overflow(&full.key_down);
    assert!(
        total > GLOBAL_HOTKEY_SLOTS,
        "前提没达成：只编出 {total} 个带 GLOBAL 的条目"
    );
    assert!(!dropped.is_empty(), "超限了却报不出来，护栏形同虚设");
    assert_eq!(
        dropped.len(),
        total - GLOBAL_HOTKEY_SLOTS,
        "报出的条数应恰好是溢出的那些"
    );
}

/// 组合键的值域就是 `BoundAction` 的全值域。
///
/// 组合键通路在按键链上位次更早（英文模式分水岭之前）、不与输入争键，本该限制最少；
/// 若有人为修某个具体问题把某一族重新挡回去，本测试会红。
#[test]
fn combo_keys_accept_the_whole_bound_action_domain() {
    let verbs = [
        // A 类状态切换
        "toggle_mode",
        "switch_engine",
        "toggle_full_width",
        "toggle_punct",
        "toggle_s2t",
        "toggle_t2s",
        "toggle_toolbar",
        "open_settings",
        "open_dictionary",
        "take_screenshot",
        // B 类进 overlay
        "temp_pinyin",
        "temp_english",
        "aux_code",
        "rare_char",
        "reverse",
        "mix:quick_mix",
        "special:fuhao",
        "single_char",
        "single_char:on",
        "comment_state",
        "comment_state:detail",
        "softkeyboard",
        "softkeyboard:num",
        // C 类切方案
        "toggle_schema:english",
        "switch_schema:wubi",
        // 命令直通
        r#"command:proc.run("charmap.exe")"#,
    ];
    for verb in verbs {
        let mut cfg = Config::default();
        cfg.keys.softkeyboard = String::new(); // 免得出厂那条占着同一个键
        cfg.keys
            .key_actions
            .insert("ctrl+alt+shift+j".to_string(), verb.to_string());
        let compiled = Compiler::new(cfg).compile();
        let raw = parse_hotkey("ctrl+alt+shift+j").expect("组合键应解析得动");
        assert!(
            compiled
                .key_down
                .iter()
                .any(|e| e.match_hash == raw && e.action == verb),
            "组合键应能绑 {verb}，且动词原样进表"
        );
    }
}

/// 策略位按动词分，且与固定字段那两段逐条一致，否则同一功能在设置页与自定义按键里配
/// 行为不同。`toggle_mode` 最容易配错：带上 `CHINESE_ONLY` 就成了单程票，切到英文后再也切不回。
#[test]
fn combo_key_policy_bits_split_by_verb() {
    // (动词, 期望 CHINESE_ONLY, 期望 GLOBAL)
    let cases: &[(&str, bool, bool)] = &[
        ("toggle_mode", false, false),
        ("switch_engine", false, false),
        ("toggle_schema:english", false, false),
        ("switch_schema:wubi", false, false),
        ("toggle_full_width", false, false),
        ("toggle_toolbar", false, false),
        ("open_settings", false, false),
        ("open_dictionary", false, false),
        ("take_screenshot", false, false),
        ("toggle_punct", true, false),
        ("toggle_s2t", true, false),
        ("toggle_t2s", true, false),
        ("single_char", true, false),
        ("comment_state", false, false),
        ("comment_state:detail", false, false),
        ("temp_pinyin", true, true),
        ("temp_english", true, true),
        ("aux_code", true, true),
        ("rare_char", true, true),
        ("reverse", true, true),
        ("mix:quick_mix", true, true),
        ("special:fuhao", true, true),
        ("softkeyboard", false, true),
        (r#"command:open("https://example.com")"#, false, false),
    ];
    for (verb, want_chinese, want_global) in cases {
        let mut cfg = Config::default();
        cfg.keys.softkeyboard = String::new();
        cfg.keys
            .key_actions
            .insert("ctrl+alt+shift+j".to_string(), (*verb).to_string());
        let compiled = Compiler::new(cfg).compile();
        let e = compiled
            .key_down
            .iter()
            .find(|e| e.action == *verb)
            .unwrap_or_else(|| panic!("{verb} 应进 key_down 表"));
        assert_eq!(
            e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
            *want_chinese,
            "{verb} 的 CHINESE_ONLY 位与预期不符"
        );
        assert_eq!(
            e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0,
            *want_global,
            "{verb} 的 GLOBAL 位与预期不符"
        );
        // match_hash 恒不含策略位：服务端匹配只认裸 hash。
        assert_eq!(
            e.match_hash & (HOTKEY_POLICY_CHINESE_ONLY | HOTKEY_POLICY_GLOBAL),
            0
        );
    }
}

/// 生僻字模式直达热键：`rare_char` 动词原样进表，策略位与 `special:` 一致
/// （CHINESE_ONLY | GLOBAL）。少了 GLOBAL，Chromium 系宿主会用自己的同名加速键吃掉这个组合。
#[test]
fn rare_char_hotkey_compiles_with_global_policy() {
    let mut cfg = Config::default();
    cfg.keys
        .key_actions
        .insert("ctrl+shift+r".to_string(), "rare_char".to_string());
    let compiled = Compiler::new(cfg).compile();
    let e = compiled
        .key_down
        .iter()
        .find(|e| e.action == "rare_char")
        .expect("key_actions 的 rare_char 应原样进表");
    assert!(
        e.tsf_hash & HOTKEY_POLICY_GLOBAL != 0,
        "须带 GLOBAL，否则 Chromium 类宿主会吃掉它"
    );
    assert!(
        e.tsf_hash & HOTKEY_POLICY_CHINESE_ONLY != 0,
        "进 overlay 只在中文输入中途有意义"
    );
}

// ── 上屏注释 / 拼音（`input.alt_commit`）──

fn alt_entries(compiled: &CompiledHotkeys) -> Vec<&HotkeyEntry> {
    compiled
        .key_down
        .iter()
        .filter(|e| (e.match_hash >> 16) == MOD_ALT)
        .collect()
}

/// 出厂关：一个 Alt 组合都不进表——Alt+数字 / Alt+空格 原样归宿主（菜单加速键、系统菜单）。
#[test]
fn alt_commit_off_registers_nothing() {
    let compiled = Compiler::new(Config::default()).compile();
    assert!(
        alt_entries(&compiled).is_empty(),
        "出厂 alt_commit=off，不该登记任何 Alt 组合"
    );
}

/// `alt_commit = comment` 而候选注释总开关关着：注释恒空，按 `off` 处理——一个 Alt 组合都不进表，
/// 否则 TSF 照 SESSION 位吃掉 Alt+数字，宿主的菜单加速键就丢了。`pinyin` 档不受影响。
#[test]
fn alt_commit_comment_with_comments_off_registers_nothing() {
    let mut cfg = Config::default();
    cfg.input.alt_commit = "comment".into();
    cfg.ui.comment.enabled = false;
    assert!(
        alt_entries(&Compiler::new(cfg.clone()).compile()).is_empty(),
        "注释关着时 comment 档等同 off，不该登记 Alt 组合"
    );
    cfg.input.alt_commit = "pinyin".into();
    assert_eq!(
        alt_entries(&Compiler::new(cfg).compile()).len(),
        11,
        "pinyin 档不看注释开关"
    );
}

/// 开启后 Alt+0..9 与 Alt+Space 共 11 个键进 key_down，只带 SESSION 位：
/// 无会话 / 英文模式一律放行（TSF 两道闸门与 `should_handle_key` 都按这个位判），
/// 候选可见期间才由 `_RegisterCandidateHotkeys` 抢占。action 为空＝仅转发，
/// 语义在协调器的 `try_alt_commit` 里分派。
#[test]
fn alt_commit_on_registers_eleven_session_keys() {
    let mut cfg = Config::default();
    cfg.input.alt_commit = "pinyin".into();
    let compiled = Compiler::new(cfg).compile();
    let alts = alt_entries(&compiled);
    let mut vks: Vec<u32> = alts.iter().map(|e| e.match_hash & 0xFFFF).collect();
    vks.sort();
    let mut want: Vec<u32> = (0x30..=0x39).collect();
    want.insert(0, VK_SPACE);
    assert_eq!(vks, want);
    for e in alts {
        assert_eq!(
            e.tsf_hash & 0xF000_0000,
            HOTKEY_POLICY_SESSION,
            "只该带 SESSION 位（不能 GLOBAL/CHINESE_ONLY/FORWARD_ONLY）: 0x{:08X}",
            e.tsf_hash
        );
        assert!(e.action.is_empty(), "仅转发登记，action 必须为空");
        assert_eq!(
            KeyDownPolicy::from_tsf_hash(e.tsf_hash),
            KeyDownPolicy::Session
        );
    }
}

/// 用户在 `keys.key_actions` 里已绑的 Alt 组合让位：本功能不登记那个键，
/// 表里只剩用户那条（`.find()` 先到先得的同时，推给 TSF 的策略位也只有一份）。
#[test]
fn alt_commit_yields_to_user_combo_binding() {
    let mut cfg = Config::default();
    cfg.input.alt_commit = "comment".into();
    cfg.keys
        .key_actions
        .insert("alt+3".into(), "toggle_punct".into());
    let compiled = Compiler::new(cfg).compile();
    let raw = key_hash(MOD_ALT, 0x33);
    let hits: Vec<&HotkeyEntry> = compiled
        .key_down
        .iter()
        .filter(|e| e.match_hash == raw)
        .collect();
    assert_eq!(hits.len(), 1, "Alt+3 只该有用户那一条");
    assert_eq!(hits[0].action, "toggle_punct");
    assert_eq!(alt_entries(&compiled).len(), 11, "其余 10 个照常登记");
}

/// 候选热键槽位（TSF `kHotkeyIdCandidateMax` = 32）装得下「出厂置顶 + 删除 + 本功能」。
/// 超出的那些 `_RegisterCandidateHotkeys` 静默 break，退回 OnTestKeyDown 退路。
#[test]
fn session_hotkeys_fit_candidate_slots_with_alt_commit() {
    let mut cfg = Config::default();
    cfg.input.alt_commit = "pinyin".into();
    let compiled = Compiler::new(cfg).compile();
    let n = compiled
        .key_down
        .iter()
        .filter(|e| e.tsf_hash & HOTKEY_POLICY_SESSION != 0)
        .count();
    assert!(n <= 32, "SESSION 热键 {n} 个，超过 TSF 候选槽位 32");
}

#[test]
fn reverse_hotkey_policy_is_global_chinese_only() {
    let p = hotkey_policy_for(&crate::config::BoundAction::Reverse).unwrap();
    assert_eq!(p & HOTKEY_POLICY_CHINESE_ONLY, HOTKEY_POLICY_CHINESE_ONLY);
    assert_eq!(p & HOTKEY_POLICY_GLOBAL, HOTKEY_POLICY_GLOBAL);
}
