//! 零回归闸门：出厂格式表、内置默认表、以及格式表化之前的硬编码行为，三者必须一致。

use wind_quick_input::{FormatTable, QuickSource, generate, generate_with};

/// 出厂 `data/system.quick.toml` 的实际路径（crate → 仓库根 → data/）。
fn factory_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/system.quick.toml")
}

/// 出厂文件与代码内置表不得漂移：文件给正常路径，`builtin()` 给文件缺失/损坏的兜底路径，
/// 只改一处的话兜底时候选会变且无报错。
#[test]
fn factory_file_matches_builtin_table() {
    let path = factory_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到出厂格式表 {}: {}", path.display(), e));
    let from_file = FormatTable::parse(&text).expect("出厂格式表必须能解析");
    assert_eq!(
        from_file,
        FormatTable::builtin(),
        "data/system.quick.toml 与 FormatTable::builtin() 不一致——两处必须同步修改"
    );
}

/// 出厂文件里的每一条都必须被接受。单条非法只会被静默剔除，「解析成功」不等于一条没丢，故比条数。
#[test]
fn factory_file_loses_no_entry() {
    let text = std::fs::read_to_string(factory_path()).unwrap();
    let parsed = FormatTable::parse(&text).unwrap();
    let declared = text.matches("[[formats]]").count();
    assert_eq!(
        parsed.entries().len(),
        declared,
        "出厂表声明了 {} 条，只有 {} 条通过校验——有条目被静默剔除",
        declared,
        parsed.entries().len()
    );
}

/// 内置表渲染出的公历候选 == 格式表化之前的硬编码输出，逐条逐序。期望值是字面量，
/// 不引用实现代码，否则闸门会随实现一起漂移。
///
/// 农历两条是有意新增、追加在公历之后，故只比前 5 条；它们由
/// [`lunar_entries_are_appended_after_solar`] 单独把关。
#[test]
fn builtin_output_equals_pre_refactor_hardcoding() {
    // 完整日期
    assert_eq!(
        generate(QuickSource::Date, "2025.12.25", 6)[..5],
        [
            "2025年12月25日",
            "二〇二五年十二月二十五日",
            "2025-12-25",
            "20251225",
            "2025/12/25"
        ]
    );
    // 月日 <10 不补零（GB/T 15835）
    assert_eq!(
        generate(QuickSource::Date, "2025.3.5", 6)[..5],
        [
            "2025年3月5日",
            "二〇二五年三月五日",
            "2025-03-05",
            "20250305",
            "2025/03/05"
        ]
    );
    // 年月。尾点不是笔误：走 `QuickSource::Date` 的输入要有第二个点才归日期
    // （一个点归数字），见 `has_second_dot`。
    assert_eq!(
        generate(QuickSource::Date, "2025.6.", 6),
        vec!["2025年6月", "二〇二五年六月", "2025-06", "2025/06"]
    );
    // 两段日期（`12.25.`）不在这道闸门里：它分出了 `month_day` 类、首选改为不带年的
    // `12月25日`，是有意的行为变更。正面覆盖见 `test_month_day_forms_prefer_short_writing`。
    // 整数：金额 → 中文小写 → 中文大写 → 逐位 → 千分位
    assert_eq!(
        generate(QuickSource::Number, "123", 6),
        vec![
            "壹佰贰拾叁元整",
            "一百二十三",
            "壹佰贰拾叁",
            "一二三",
            "123"
        ]
    );
    // 两位小数：金额含角分
    assert_eq!(
        generate(QuickSource::Number, "123.45", 6),
        vec![
            "壹佰贰拾叁元肆角伍分",
            "一百二十三点四五",
            "壹佰贰拾叁点肆伍",
            "一二三点四五",
            "123.45"
        ]
    );
    // 计算：结果首选、等式次之
    assert_eq!(
        generate(QuickSource::Calc, "1+2*3", 6),
        vec!["7", "1+2*3=7"]
    );
}

/// 农历两条追加在公历五条之后——顺序错了就会挤掉首选。
#[test]
fn lunar_entries_are_appended_after_solar() {
    assert_eq!(
        generate(QuickSource::Date, "2025.12.25", 6),
        vec![
            "2025年12月25日",
            "二〇二五年十二月二十五日",
            "2025-12-25",
            "20251225",
            "2025/12/25",
            "农历冬月初六",
            "乙巳年冬月初六",
        ]
    );
    // 闰月要带「闰」字
    assert_eq!(
        generate(QuickSource::Date, "2020.6.1", 6)[5..],
        ["农历闰四月初十", "庚子年闰四月初十"]
    );
    // 干支按农历年而非公历年：2026-01-01 仍在乙巳年
    assert_eq!(
        generate(QuickSource::Date, "2026.1.1", 6)[6],
        "乙巳年冬月十三"
    );
}

/// 农历算不出时，那两条消失而公历五条照常。农历变量取不到值返回 `None` 而非空串，
/// 否则 `农历$LMD` 会剩下「农历」二字上屏。
#[test]
fn out_of_range_date_keeps_solar_entries_only() {
    let got = generate(QuickSource::Date, "1899.12.31", 6);
    assert_eq!(
        got,
        vec![
            "1899年12月31日",
            "一八九九年十二月三十一日",
            "1899-12-31",
            "18991231",
            "1899/12/31"
        ],
        "超范围时只剩公历五条，且不得出现「农历」「年」这类半截文本"
    );
    assert!(!got.iter().any(|s| s.contains("农历")));
}

/// 年月类不出农历——农历月与公历月不一一对应。
#[test]
fn year_month_has_no_lunar_candidate() {
    let got = generate(QuickSource::Date, "2025.6.", 6);
    assert_eq!(
        got,
        vec!["2025年6月", "二〇二五年六月", "2025-06", "2025/06"]
    );
}

/// 三位小数无角分写法：那一条候选消失（`$AMT` 渲染为空串 → 丢弃），其余不受影响。
#[test]
fn amount_entry_vanishes_beyond_two_decimals() {
    let got = generate(QuickSource::Number, "1.234", 6);
    assert!(
        !got.iter().any(|s| s.contains('元')),
        "三位小数不应有金额候选，实际: {:?}",
        got
    );
    assert_eq!(
        got,
        vec!["一点二三四", "壹点贰叁肆", "一点二三四", "1.234"],
        "其余四条照常（中文小写与逐位在此输入下恰好同形）"
    );
}

/// 自定义表真的生效：换模板、换顺序、只留一条，三种改法都要看得见。
#[test]
fn custom_table_changes_text_and_order() {
    let table = FormatTable::parse(
        r#"
[[formats]]
id = "date.us"
kind = "date"
text = "$MM/$DD/$YYYY"
position = 2

[[formats]]
id = "date.dotted"
kind = "date"
text = "$YYYY.$MM.$DD"
position = 1
"#,
    )
    .unwrap();
    assert_eq!(
        generate_with(QuickSource::Date, "2025.12.25", 6, &table),
        vec!["2025.12.25", "12/25/2025"],
        "position 决定组内顺序，与文件出现序无关"
    );
}

/// 用户可以把某一类整个删空——那一类就不出候选，且不会连累其它类。
#[test]
fn empty_kind_yields_nothing_without_affecting_others() {
    let table = FormatTable::parse(
        r#"
[[formats]]
id = "calc.only"
kind = "calc"
text = "$RESULT"
"#,
    )
    .unwrap();
    assert!(generate_with(QuickSource::Date, "2025.12.25", 6, &table).is_empty());
    assert!(generate_with(QuickSource::Number, "123", 6, &table).is_empty());
    assert_eq!(
        generate_with(QuickSource::Calc, "1+1", 6, &table),
        vec!["2"]
    );
}
