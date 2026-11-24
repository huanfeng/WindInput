use super::*;

fn cfg() -> InputConfig {
    InputConfig::default()
}

#[test]
fn smart_punct_after_digit_default_list() {
    let c = cfg(); // 默认 smart_punct_after_digit=true, list=".,:"
    // '.' 在列表 + 前字符是数字 '5'(0x35) → true
    assert!(is_smart_punct_after_digit(&c.punct, '.', 0x35));
    // 前字符非数字 → false
    assert!(!is_smart_punct_after_digit(&c.punct, '.', b'a' as u16));
    // 不在列表的标点 → false
    assert!(!is_smart_punct_after_digit(&c.punct, '!', 0x35));
}

#[test]
fn smart_punct_after_latin_needs_switch_list_and_latin_before() {
    let mut c = cfg();
    assert!(!c.punct.smart_after_latin, "出厂关");
    assert!(!is_smart_punct_after_latin(&c.punct, '.', true));
    c.punct.smart_after_latin = true;
    assert!(is_smart_punct_after_latin(&c.punct, '.', true));
    assert!(is_smart_punct_after_latin(&c.punct, '_', true));
    assert!(
        !is_smart_punct_after_latin(&c.punct, ',', true),
        "出厂列表不含逗号"
    );
    assert!(
        !is_smart_punct_after_latin(&c.punct, '.', false),
        "前一字符不是字母"
    );
}

#[test]
fn convert_punct_ctx_latin_before_goes_english() {
    let mut conv = PunctuationConverter::new();
    let mut c = cfg();
    c.punct.smart_after_latin = true;
    assert_eq!(
        convert_punct_ctx(&mut conv, &c.punct, true, false, '.', 0, true),
        "."
    );
    assert_eq!(
        convert_punct_ctx(&mut conv, &c.punct, true, false, '_', 0, true),
        "_"
    );
    assert_eq!(
        convert_punct_ctx(&mut conv, &c.punct, true, false, '.', 0, false),
        "。"
    );
    // 逗号不在出厂列表：英文后照出中文逗号（「我用 Python，」）。
    assert_eq!(
        convert_punct_ctx(&mut conv, &c.punct, true, false, ',', 0, true),
        "，"
    );
    // 英文标点状态本来就出英文，不受影响。
    assert_eq!(
        convert_punct_ctx(&mut conv, &c.punct, false, false, '.', 0, true),
        "."
    );
}

#[test]
fn convert_punct_chinese_and_fullwidth() {
    let mut conv = PunctuationConverter::new();
    let c = cfg();
    // 中文标点模式：'.' → '。'
    assert_eq!(
        convert_punct(&mut conv, &c.punct, true, false, '.', 0),
        "。"
    );
    // 英文标点模式 + 全角：'.' 走全半角 → '．'
    let out = convert_punct(&mut conv, &c.punct, false, true, '.', 0);
    assert_ne!(out, "."); // 全角化
}

#[test]
fn convert_punct_smart_digit_forces_english() {
    let mut conv = PunctuationConverter::new();
    let c = cfg();
    // 中文模式但前字符是数字 → '.' 按英文输出（不转 '。'）。
    assert_eq!(
        convert_punct(&mut conv, &c.punct, true, false, '.', 0x33),
        "."
    );
}

#[test]
fn compute_pure_mirrors_chinese() {
    let conv = PunctuationConverter::new();
    let c = cfg();
    // 中文产物：'.' → '。'（peek 不改状态）。
    assert_eq!(
        compute_punct_str_pure(&conv, &c.punct, false, '.', true).as_deref(),
        Some("。")
    );
}

#[test]
fn participates_substring_match() {
    let mut c = cfg();
    c.symbol.smart_chars = "。，".to_string();
    assert!(participates(&c, "。"));
    assert!(!participates(&c, "！"));
    assert!(!participates(&c, ""));
}

/// 英文侧参与判定按**源字符**（键本身的 ASCII），与中文侧按产物刻意不同。
#[test]
fn english_participates_by_source_char() {
    let mut c = cfg();
    c.symbol.english_chars = ".,".to_string();
    assert!(english_participates(&c, '.'));
    assert!(english_participates(&c, ','));
    assert!(!english_participates(&c, '?'));
}

/// 空白不参与，与 `english_smart_source_chars` 的过滤同源：空格键在解除武装判据里答 None，
/// 参与的话 press1 会自解武装、press2 永不到来。
#[test]
fn english_participates_never_matches_whitespace() {
    let mut c = cfg();
    c.symbol.english_chars = ". ,".to_string();
    assert!(!english_participates(&c, ' '));
    assert!(english_participates(&c, '.'), "非空白成员不受影响");
}

/// 推给 DLL 的吃键集受 `english_mode` 门控：关闭时必须是空集——英文模式的标点键就该
/// 保持透传，多吃一个键就是一次潜在丢键（吃了再吐，严格 TSF 宿主不回退合成 WM_CHAR）。
#[test]
fn english_smart_source_chars_gated_by_switch() {
    let mut c = cfg();
    c.symbol.english_chars = ".,;".to_string();
    assert!(
        english_smart_source_chars(&c).is_empty(),
        "开关关闭时不得吃任何键"
    );
    c.symbol.english_mode = true;
    // 升序去重（推送字节须可复现）。
    assert_eq!(english_smart_source_chars(&c), vec![',', '.', ';']);
    // 空白不是按键产物，不进吃键集。
    c.symbol.english_chars = ". ,".to_string();
    assert_eq!(english_smart_source_chars(&c), vec![',', '.']);
}

#[test]
fn custom_space_cols_marks_mapped_columns() {
    let conv = PunctuationConverter::new();
    let mut c = cfg();
    assert_eq!(custom_space_cols(&conv, &c.punct), 0, "出厂无映射");
    c.punct.custom_enabled = true;
    // 列序：0=中半 1=英全 2=中全 3=英半；空串 = 回落默认，不算配了。
    c.punct.custom_mappings.insert(
        " ".into(),
        vec!["\u{3000}".into(), "".into(), "\u{3000}".into(), "".into()],
    );
    assert_eq!(custom_space_cols(&conv, &c.punct), 0b0101);
    c.punct.custom_enabled = false;
    assert_eq!(custom_space_cols(&conv, &c.punct), 0, "总开关关着不算");
}

#[test]
fn custom_lookup_empty_is_none() {
    let conv = PunctuationConverter::new();
    let c = cfg(); // 默认无自定义映射
    assert_eq!(custom_lookup(&conv, &c.punct, '.', 0), None);
}

/// 自定义映射来自实时配置而非转换器里的启动快照：同一个 conv 实例，配置加上映射后
/// 下一次转换即生效。存快照的话设置页改自定义标点要重启服务才生效。
#[test]
fn convert_punct_follows_live_custom_mappings() {
    let mut conv = PunctuationConverter::new();
    let mut c = cfg();
    // 出厂：中文标点模式下 '"' 走内置引号交替 → 左引号。
    assert_eq!(
        convert_punct(&mut conv, &c.punct, true, false, '"', 0),
        "\u{201C}"
    );
    conv.reset();

    // 用户在设置页配了双引号第一次/第二次（中文半角列）。
    c.punct.custom_enabled = true;
    c.punct
        .custom_mappings
        .insert("\"1".into(), vec!["「".into()]);
    c.punct
        .custom_mappings
        .insert("\"2".into(), vec!["」".into()]);
    assert_eq!(
        convert_punct(&mut conv, &c.punct, true, false, '"', 0),
        "「",
        "热重载后第一次应立即出自定义值"
    );
    assert_eq!(
        convert_punct(&mut conv, &c.punct, true, false, '"', 0),
        "」",
        "第二次应出「第二次」那一行"
    );
}

/// 推给 DLL 的吃键集合：只含「英半列非空」的行的源字符，且引号两行折回同一字符。
/// 这个集合同时是 DLL 的吃键判据和 core 的出字判据，多一个字符就是一次丢键。
#[test]
fn custom_english_punct_chars_only_covered_keys() {
    let mut c = cfg();
    c.punct.custom_enabled = true;
    // 引号两行都配了英半列 → 折回一个 '"'
    c.punct.custom_mappings.insert(
        "\"1".into(),
        vec!["E".into(), "".into(), "".into(), "#".into()],
    );
    c.punct.custom_mappings.insert(
        "\"2".into(),
        vec!["￥".into(), "".into(), "".into(), "$".into()],
    );
    // 只配了中半列 → 英文模式无需吃键
    c.punct
        .custom_mappings
        .insert("/".into(), vec!["、".into()]);
    // 英半列是空串（回落默认）→ 同样不该吃
    c.punct.custom_mappings.insert(
        ";".into(),
        vec!["；".into(), "".into(), "".into(), "".into()],
    );
    // 单引号只配英半列
    c.punct.custom_mappings.insert(
        "'1".into(),
        vec!["".into(), "".into(), "".into(), "@".into()],
    );
    // 英半列就是原字符本身（设置端整行落盘的默认值）→ 没有改写，不该吃
    c.punct.custom_mappings.insert(
        "-".into(),
        vec!["－".into(), "－".into(), "－".into(), "-".into()],
    );
    assert_eq!(custom_english_punct_chars(&c.punct), vec!['"', '\'']);

    // 总开关关掉 → 空集合（DLL 恢复历史行为）
    c.punct.custom_enabled = false;
    assert!(custom_english_punct_chars(&c.punct).is_empty());
}

/// `"1`/`"2` 两行 = 左形/右形：配对判定与插入都从这里取，两行都用得上。
#[test]
fn quote_forms_maps_two_rows_to_left_and_right() {
    let mut c = cfg();
    // 未自定义：回落内置中文引号。
    assert_eq!(
        quote_forms(&c.punct, true, false, '"'),
        Some(("\u{201C}".into(), "\u{201D}".into()))
    );
    // 两行齐：左形取 "1、右形取 "2。
    c.punct.custom_enabled = true;
    c.punct
        .custom_mappings
        .insert("\"1".into(), vec!["「".into()]);
    c.punct
        .custom_mappings
        .insert("\"2".into(), vec!["」".into()]);
    assert_eq!(
        quote_forms(&c.punct, true, false, '"'),
        Some(("「".into(), "」".into()))
    );
    // 只配左形：右侧回落内置（不会跟着变）。
    c.punct.custom_mappings.remove("\"2");
    assert_eq!(
        quote_forms(&c.punct, true, false, '"'),
        Some(("「".into(), "\u{201D}".into()))
    );
    // 非引号键无左右形。
    assert_eq!(quote_forms(&c.punct, true, false, ','), None);
    // 列随模式走：中文全角取第 2 列。
    c.punct
        .custom_mappings
        .insert("\"1".into(), vec!["「".into(), "x".into(), "『".into()]);
    assert_eq!(
        quote_forms(&c.punct, true, true, '"').map(|(l, _)| l),
        Some("『".into())
    );
}

/// 引号在 pure 路径也能查到自定义（键取 `"1`/`"2`），且 peek 不推进交替态。
/// 这决定智能符号的武装判定拿的是「用户实际会上屏的符号」而非标准引号。
#[test]
fn compute_pure_quote_uses_custom_mapping() {
    let conv = PunctuationConverter::new();
    let mut c = cfg();
    c.punct.custom_enabled = true;
    c.punct
        .custom_mappings
        .insert("\"1".into(), vec!["￥".into()]);
    assert_eq!(
        compute_punct_str_pure(&conv, &c.punct, false, '"', true).as_deref(),
        Some("￥")
    );
    // 英文半角列刻意不查自定义（press2 的替换目标须保持原样英文）。
    assert_eq!(
        compute_punct_str_pure(&conv, &c.punct, false, '"', false).as_deref(),
        Some("\"")
    );
}

// ── chinese_passthrough_punct_chars ────────────────────────────────────────
//
// 判据的真值表锁在这里。漏掉任一条排除项的表现是「某个键忽然不经引擎了」且不报错。

#[test]
fn passthrough_default_is_the_unmapped_punct_set() {
    let conv = PunctuationConverter::new();
    let c = cfg();
    // 默认配置下「标点层面产物不变」的全集，恰是中文标点表没有映射的那十个。
    // 这张表同时是撞码风险的全集（ASCII 码点 ≤0xFF 才可能撞 VK）：
    //   `#`→VK_END  `%`→VK_LEFT  `&`→VK_UP  `-`→VK_INSERT  `/`→VK_HELP  `|`→VK_F13
    //   `@` `=` 落在未分配 VK 上，`*`→VK_PRINT、`+`→VK_EXECUTE 无默认绑定。
    // 这不是最终放行集：`-` `=` `/` 还可能被配成引导键或方案码元首码，由 `ConfigBundle` 再滤一道。
    assert_eq!(
        chinese_passthrough_punct_chars(&conv, &c),
        vec!['#', '%', '&', '*', '+', '-', '/', '=', '@', '|']
    );
}

#[test]
fn english_passthrough_is_superset_of_chinese() {
    // 出厂配置下英文态是中文态的超集：英文态不走中文标点表，中文态能透传的它也能。
    // 反过来不成立——`,` `.` `;` 这些在中文态要转、在英文态却是原样。
    // 只在出厂配置下成立，不是不变量：下面的反例说明配置可以打破它，别据此去「修」判据。
    let conv = PunctuationConverter::new();
    let c = cfg();
    let cn = chinese_passthrough_punct_chars(&conv, &c);
    let en = english_passthrough_punct_chars(&c);
    for ch in &cn {
        assert!(en.contains(ch), "`{ch}` 中文态能透传，英文态没有理由不能");
    }
    assert!(
        en.len() > cn.len(),
        "出厂配置下英文态该是真超集，不该只是相等"
    );

    // 反例：`@` 无中文映射（中文态可透传），但配进英文智能符号并开开关后，英文态
    // 必须把它留给引擎 ⇒ 超集关系被打破，而这是正确行为。
    let mut c2 = cfg();
    c2.symbol.english_punct_mode = true;
    c2.symbol.english_chars = "@".into();
    let cn2 = chinese_passthrough_punct_chars(&conv, &c2);
    let en2 = english_passthrough_punct_chars(&c2);
    assert!(cn2.contains(&'@'), "中文态不看 english_chars，`@` 仍可透传");
    assert!(!en2.contains(&'@'), "英文态开了开关，`@` 要留给智能符号");
}

#[test]
fn english_passthrough_default_set() {
    // 默认配置下被挡住的只有配对符一类：出厂 `english_punct_mode = false`，
    // 智能符号那条判据不成立。
    let en = english_passthrough_punct_chars(&cfg());
    // `.` 撞 `VK_DELETE`（会吞掉光标后一个字符）。判据要看开关而非只看 `english_chars`，
    // 否则出厂关闭的功能会把这批键挡在透传外。
    for ch in ['.', ',', '?', '!', ':', ';'] {
        assert!(
            en.contains(&ch),
            "`{ch}` 在出厂配置（english_punct_mode=false）下产物即原样，应透传"
        );
    }
    // 开关打开后才轮到智能符号接手。
    let mut c_sw = cfg();
    c_sw.symbol.english_punct_mode = true;
    let en_sw = english_passthrough_punct_chars(&c_sw);
    for ch in ['.', ',', '?', '!', ':', ';'] {
        assert!(
            !en_sw.contains(&ch),
            "开了 english_punct_mode，`{ch}` 要留给引擎"
        );
    }
    // 出厂 `auto_pair.english = false`，配对没开 ⇒ 配对符也该透传。
    // `(`(0x28) 撞 `VK_DOWN`、`)`(0x29) 撞 `VK_SELECT`，挡住会表现为打 `(` 光标下移、字不上屏。
    for ch in ['(', ')', '[', ']', '{', '}'] {
        assert!(
            en.contains(&ch),
            "`{ch}` 在出厂配置（auto_pair.english=false）下不参与配对，应透传"
        );
    }
    // 开关打开后才轮到配对栈接手。
    let mut c2 = cfg();
    c2.auto_pair.english = true;
    let en2 = english_passthrough_punct_chars(&c2);
    for ch in ['(', ')', '[', ']', '{', '}'] {
        assert!(!en2.contains(&ch), "开了配对，`{ch}` 要留给引擎维护配对栈");
    }
    // 中文态里被中文标点表拦下、英文态却该放行的那批，逐个点名。
    for ch in ['\'', '"', '\\', '`', '~', '$', '^', '_'] {
        assert!(en.contains(&ch), "`{ch}` 在英文半角态产物即原样，应透传");
    }
}

#[test]
fn english_passthrough_excludes_en_half_custom() {
    // 英半列配了自定义 ⇒ 要出用户配的值，必须吃。判据复用
    // `custom_english_punct_chars`，与 DLL 英文模式吃键同源。
    let mut c = cfg();
    c.punct.custom_enabled = true;
    c.punct.custom_mappings.insert(
        "/".into(),
        vec![String::new(), String::new(), String::new(), "÷".into()],
    );
    assert!(!english_passthrough_punct_chars(&c).contains(&'/'));
}

#[test]
fn passthrough_excludes_quotes_via_state_machine() {
    // 引号不在 `static_chinese` 表里、走交替状态机，但 `peek_chinese_str` 覆盖了它们
    // （返回 `‘` / `“`），故判据天然把它们排除。漏掉这条的后果是引号被透传、
    // 中文引号再也打不出来。
    let conv = PunctuationConverter::new();
    let got = chinese_passthrough_punct_chars(&conv, &cfg());
    assert!(!got.contains(&'\''), "单引号要转中文引号，不得透传");
    assert!(!got.contains(&'"'), "双引号要转中文引号，不得透传");
}

#[test]
fn passthrough_excludes_chinese_mapped_symbols() {
    let conv = PunctuationConverter::new();
    let c = cfg();
    let got = chinese_passthrough_punct_chars(&conv, &c);
    for ch in ['!', '$', '^', '(', ')'] {
        assert!(!got.contains(&ch), "{ch} 有中文标点映射，不得透传");
    }
}

#[test]
fn passthrough_excludes_any_custom_mapping_column() {
    // 中半列配了 → 必须吃（要出用户配的值）。
    let conv = PunctuationConverter::new();
    let mut c = cfg();
    c.punct.custom_enabled = true;
    c.punct.custom_mappings.insert(
        "#".into(),
        vec!["井".into(), String::new(), String::new(), String::new()],
    );
    assert!(!chinese_passthrough_punct_chars(&conv, &c).contains(&'#'));

    // ★ 只配了**英半列**也必须吃：中文输入模式下还能切到英文标点态走那一列，
    //   透传掉的话用户配的那格就永远打不出来。这条是「只看中半列」会漏掉的。
    let mut c2 = cfg();
    c2.punct.custom_enabled = true;
    c2.punct.custom_mappings.insert(
        "%".into(),
        vec![String::new(), String::new(), String::new(), "pct".into()],
    );
    assert!(!chinese_passthrough_punct_chars(&conv, &c2).contains(&'%'));
}

#[test]
fn passthrough_empty_custom_column_is_not_coverage() {
    // 四列全是空串 = 回落默认转换，不算覆盖，仍可透传。
    let conv = PunctuationConverter::new();
    let mut c = cfg();
    c.punct.custom_enabled = true;
    c.punct.custom_mappings.insert(
        "#".into(),
        vec![String::new(), String::new(), String::new(), String::new()],
    );
    assert!(chinese_passthrough_punct_chars(&conv, &c).contains(&'#'));
}

#[test]
fn passthrough_excludes_smart_symbol_and_pair_chars() {
    let conv = PunctuationConverter::new();
    // 参与**中文**智能符号**且开关开着** → 连按替换要引擎接手。中文态按**产物**判，
    // 而走到这一步的字符产物就是它自己，故把 ASCII 配进 `smart_chars` 即命中。
    let mut c = cfg();
    c.symbol.smart_mode = true;
    c.symbol.smart_chars = "#".into();
    assert!(!chinese_passthrough_punct_chars(&conv, &c).contains(&'#'));

    // `smart_mode` 关着时智能符号不生效，光有 `smart_chars` 不该挡住透传
    // （出厂 `smart_mode = false` 而 `smart_chars` 有值）。
    let mut c_off = cfg();
    c_off.symbol.smart_mode = false;
    c_off.symbol.smart_chars = "#".into();
    assert!(
        chinese_passthrough_punct_chars(&conv, &c_off).contains(&'#'),
        "smart_mode 关着，光有 smart_chars 不该挡住透传"
    );

    // `english_chars` 不该影响中文态：它归 english_punct_mode / english_mode 管。
    // 按它排除会把出厂 `.,?!:;` 整排挡在透传外。
    let mut c2 = cfg();
    c2.symbol.english_chars = "#".into();
    assert!(
        chinese_passthrough_punct_chars(&conv, &c2).contains(&'#'),
        "english_chars 不该影响中文标点态的判据"
    );

    // 被配进配对表**且开关开着** → 配对栈由引擎维护，必须吃。
    let mut c2 = cfg();
    c2.auto_pair.english = true;
    c2.auto_pair.english_pairs = vec!["@&".into()];
    let got = chinese_passthrough_punct_chars(&conv, &c2);
    assert!(!got.contains(&'@'));
    assert!(!got.contains(&'&'));

    // 开关关着时配对不生效，光有表不该挡住透传（两个开关出厂都是 false）。
    let mut c3 = cfg();
    c3.auto_pair.english = false;
    c3.auto_pair.english_pairs = vec!["@&".into()];
    let got3 = chinese_passthrough_punct_chars(&conv, &c3);
    assert!(got3.contains(&'@'), "配对开关关着，光有表不该挡住透传");
}
