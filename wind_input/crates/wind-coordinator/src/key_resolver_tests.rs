use super::*;

fn table(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// 单键进表、组合键不进表——两条通路的分流在预编译期就该完成。
#[test]
fn only_single_keys_are_compiled() {
    let t = compile_global_lead(&table(&[
        ("backtick", "temp_pinyin"),
        ("ctrl+alt+e", "temp_english"),
    ]));
    let backtick = key_action_name_to_vk("backtick").expect("backtick 应能解析");
    assert_eq!(t.get(&backtick), Some(&BoundAction::TempPinyin));
    assert_eq!(t.len(), 1, "组合键不该进引导键表：{t:?}");
}

/// 纯修饰键必须进表——它的键名不在 `KEY_TABLE` 里，只能靠 `modifier_name_to_vk`。
/// 少这一层的表现是「TSF 发了 keyup 但查表查不到」。
#[test]
fn modifier_keys_are_compiled() {
    let t = compile_global_lead(&table(&[("rshift", "toggle_schema:english")]));
    let rshift = keymap::modifier_name_to_vk("rshift").expect("rshift 应能解析");
    assert_eq!(
        t.get(&rshift),
        Some(&BoundAction::ToggleSchema("english".to_string()))
    );
}

/// ★ 显式 `none` 必须**留在表里**而不是被过滤掉：它表示「全局层表了态：禁用」，
/// 查得到才能阻止往下回落到 `z_key_action`。若过滤掉，配了 `z = "none"` 的用户
/// 会落回 z 专用字段，等于开关失灵。
#[test]
fn explicit_none_stays_in_table() {
    let t = compile_global_lead(&table(&[("z", "none")]));
    let z = key_action_name_to_vk("z").expect("z 应能解析");
    assert_eq!(t.get(&z), Some(&BoundAction::None), "显式 none 不能被丢掉");
}

/// ★ 撞键时由**字典序在前**者获胜，与折算前的 `for` + 首个命中即 return 同序。
/// 用 `insert` 代替 `or_insert` 会让后者覆盖前者——反向且静默。
#[test]
fn duplicate_vk_first_wins_by_name_order() {
    // BTreeMap 里 "Z" < "z"（大写 ASCII 在前），而键名解析大小写不敏感 ⇒ 两条撞同一个 VK。
    //
    // ★ 前置条件写成**断言**而不是 `if` 包住断言：后者在解析规则变成大小写敏感的那天
    // 会静默跳过、用例照绿，而它恰恰是本用例唯一想测的东西。
    let z = key_action_name_to_vk("z").expect("z 应能解析");
    assert_eq!(
        key_action_name_to_vk("Z"),
        Some(z),
        "前置条件：两种写法须撞同一个 VK，否则本用例根本测不到撞键"
    );
    let t = compile_global_lead(&table(&[("Z", "temp_pinyin"), ("z", "temp_english")]));
    assert_eq!(
        t.get(&z),
        Some(&BoundAction::TempPinyin),
        "撞键应由字典序在前的 \"Z\" 获胜（与折算前的线性遍历同序）"
    );
}

/// 认不出的键名跳过，不 panic 也不进表。
#[test]
fn unknown_key_name_is_skipped() {
    let t = compile_global_lead(&table(&[("no_such_key", "temp_pinyin")]));
    assert!(t.is_empty(), "认不出的键名不该进表：{t:?}");
}

// ── 方案级会话态查表 ──

/// ★★ 方案级显式 `none` 是**表态**，必须返回 `Some(None)`。
///
/// 返回 `Option::None` 会让调用方以为「本方案没提这个键」而回落全局，于是用户写的
/// 禁用完全失效。而靠「从 override 里删掉那一行」是禁不掉的——`merge_toml` 只能
/// 新增/覆盖。这条一旦被「简化」成过滤掉 none，症状是「方案里禁了还是会翻页」。
#[test]
fn schema_session_explicit_none_is_a_stated_position() {
    let k = keymap::session_key_name_to_vk("minus").expect("minus 应能解析");
    assert_eq!(
        schema_session_lookup(&table(&[("minus", "none")]), k.vk, false, true),
        Some(wind_config::SessionAction::None),
        "显式 none 必须表态，否则调用方会回落全局"
    );
}

/// 方案没提的键返回 `None`，调用方据此回落全局。
#[test]
fn schema_session_absent_key_falls_through() {
    let k = keymap::session_key_name_to_vk("minus").expect("minus 应能解析");
    assert_eq!(
        schema_session_lookup(&table(&[("equal", "page_next")]), k.vk, false, true),
        None,
        "方案没提的键必须回落全局"
    );
}

/// ★ `include_printable = false` 时可打印键让位——临英/快捷输入要这些字符本身。
/// 谓词与全局表的 `KeyBinds::classify` 同源（`SessionKey::matches`），此处守的是
/// 方案级这条路没有绕开它。
#[test]
fn schema_session_printable_yields_in_text_modes() {
    let t = table(&[("minus", "page_prev")]);
    let k = keymap::session_key_name_to_vk("minus").expect("minus 应能解析");
    assert!(
        k.printable,
        "前置条件：minus 必须是可打印键，否则本用例测不到让位"
    );
    assert_eq!(
        schema_session_lookup(&t, k.vk, false, true),
        Some(wind_config::SessionAction::PagePrev(
            wind_config::PageOverflow::Global
        ))
    );
    assert_eq!(
        schema_session_lookup(&t, k.vk, false, false),
        None,
        "文本模式下可打印键必须让位给输入字符"
    );
}

/// `shift+` 前缀参与匹配：`shift+tab` 与 `tab` 是两条不同的绑定。
#[test]
fn schema_session_shift_prefix_is_part_of_the_key() {
    let t = table(&[("shift+tab", "page_prev")]);
    let k = keymap::session_key_name_to_vk("shift+tab").expect("shift+tab 应能解析");
    assert!(k.shift, "前置条件：shift+ 前缀须被解析出来");
    assert_eq!(
        schema_session_lookup(&t, k.vk, true, true),
        Some(wind_config::SessionAction::PagePrev(
            wind_config::PageOverflow::Global
        ))
    );
    assert_eq!(
        schema_session_lookup(&t, k.vk, false, true),
        None,
        "没按 shift 时不该命中 shift+tab"
    );
}
