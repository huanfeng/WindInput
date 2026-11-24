use super::*;

fn sample() -> UserSettings {
    UserSettings {
        adjust: vec![
            (
                FormatKind::Date,
                FormatAdjust {
                    moved: vec![("date.lunar".into(), 0), ("date.iso".into(), 2)],
                    disabled: vec!["date.basic".into()],
                    added: Vec::new(),
                },
            ),
            (
                FormatKind::Number,
                FormatAdjust {
                    moved: vec![],
                    disabled: vec!["number.digits".into()],
                    added: Vec::new(),
                },
            ),
        ],
        formats: vec![],
    }
}

/// ★★ 往返是这个模块的全部职责：导出的文件必须能被自己读回来，且**一字不差**。
#[test]
fn roundtrip_preserves_everything() {
    let s = sample();
    let text = serialize_user_settings(&s);
    let back = parse_user_settings(&text).expect("导出的文件必须能解析");
    assert!(back.skipped.is_empty(), "自产文件不该有跳过项");
    assert_eq!(back.settings, s);
}

/// ★★ `moved` 的**顺序**必须原样保留：它是 LIFO 列表（index 0 = 最新 = 优先级最高），
/// 顺序反了，用户导入后会发现「我最后调的那条被更早的调整顶掉了」。
#[test]
fn moved_order_survives_roundtrip() {
    let s = sample();
    let text = serialize_user_settings(&s);
    let back = parse_user_settings(&text).unwrap();
    let (_, a) = back
        .settings
        .adjust
        .iter()
        .find(|(k, _)| *k == FormatKind::Date)
        .unwrap();
    assert_eq!(a.moved[0].0, "date.lunar", "最新的仍在队首");
    assert_eq!(a.moved[1].0, "date.iso");
}

/// ⚠️ 模板含单引号（`{amt(unit='圆')}` 是出厂就有的写法）时必须改用双引号，
/// 否则导出的文件是语法错误的 TOML，而导出时不会报错，只在导入时炸。
#[test]
fn single_quote_in_template_survives_roundtrip() {
    let s = UserSettings {
        adjust: vec![],
        formats: vec![FormatEntry {
            id: "number.yuan".into(),
            kind: FormatKind::Number,
            text: "{amt(unit='圆')}".into(),
            position: 9,
        }],
    };
    let text = serialize_user_settings(&s);
    let back = parse_user_settings(&text).expect("含单引号的模板必须能解析回来");
    assert_eq!(back.settings.formats[0].text, "{amt(unit='圆')}");
}

/// 空设置导出后仍是一份合法（只有注释）的文件，解析回来为空。
#[test]
fn empty_settings_export_is_valid_toml() {
    let text = serialize_user_settings(&UserSettings::default());
    let back = parse_user_settings(&text).unwrap();
    assert!(back.settings.is_empty());
}

/// 全空的 adjust 段不写进文件（写了也没用，只让人以为有改动）。
#[test]
fn empty_adjust_sections_are_omitted() {
    let s = UserSettings {
        adjust: vec![(FormatKind::Date, FormatAdjust::default())],
        formats: vec![],
    };
    let text = serialize_user_settings(&s);
    assert!(!text.contains("[[adjust]]"), "空调整不该写出来:\n{text}");
}

/// 未知类别与非法模板**逐条跳过并给出原因**，不是整份失败、也不是静默丢弃。
#[test]
fn bad_entries_are_reported_not_swallowed() {
    let text = r#"
[[formats]]
id = 'x.weather'
kind = 'weather'
text = '$Y'

[[formats]]
id = 'ym.bad'
kind = 'year_month'
text = '$Y年$M月$D日'

[[adjust]]
kind = 'nope'
disabled = ['a']

[[adjust]]
kind = 'date'
disabled = ['date.basic']
"#;
    let out = parse_user_settings(text).unwrap();
    assert_eq!(
        out.skipped.len(),
        3,
        "两条坏条目 + 一个坏类别: {:?}",
        out.skipped
    );
    assert!(out.skipped.iter().any(|s| s.contains("weather")));
    // year_month 不支持 $D——白名单在 kind 上，这正是最容易「设了没反应」的地方
    assert!(
        out.skipped.iter().any(|s| s.contains("$D")),
        "{:?}",
        out.skipped
    );
    // 好的那段照常收下
    assert_eq!(out.settings.adjust.len(), 1);
    assert_eq!(out.settings.adjust[0].0, FormatKind::Date);
}

/// 同一类别写两段：合并，不是后者顶替前者（丢一段就是静默截断）。
#[test]
fn duplicate_kind_sections_merge() {
    let text = r#"
[[adjust]]
kind = 'date'
disabled = ['a']

[[adjust]]
kind = 'date'
disabled = ['b']
"#;
    let out = parse_user_settings(text).unwrap();
    assert_eq!(out.settings.adjust.len(), 1);
    assert_eq!(out.settings.adjust[0].1.disabled, vec!["a", "b"]);
}

/// 整份语法错误才返回 Err（调用方据此提示「这不是一份有效文件」）。
#[test]
fn syntax_error_is_an_error() {
    assert!(parse_user_settings("[[adjust]]\nkind = ").is_err());
}

/// 空文件 / 不相关的 TOML 解析为空设置而非报错——用户选错文件时，
/// 「导入了 0 条」比「解析失败」更容易看懂。
#[test]
fn unrelated_toml_parses_as_empty() {
    let out = parse_user_settings("[something]\nkey = 1\n").unwrap();
    assert!(out.settings.is_empty());
}
