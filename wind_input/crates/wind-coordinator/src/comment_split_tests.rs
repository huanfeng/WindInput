use super::*;

fn ev<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
    move |n, _arg| {
        pairs
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.to_string())
    }
}

fn split(tpl: &str, vars: &[(&str, &str)]) -> (String, String) {
    let (a, b) = Template::parse(tpl).render_split(0, &ev(vars));
    (a.into_string(), b.into_string())
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[test]
fn split_on_first_literal_newline() {
    assert_eq!(
        split(
            "${pinyin}\n${chaizi}",
            &[("pinyin", "ni hao"), ("chaizi", "亻尔")]
        ),
        pair("ni hao", "亻尔")
    );
}

/// 字面 `\n`（旧版单行框手写的写法）与真换行等价。
#[test]
fn literal_backslash_n_is_newline() {
    assert_eq!(
        split(
            "${pinyin}\\n${chaizi}",
            &[("pinyin", "ni"), ("chaizi", "亻尔")]
        ),
        split(
            "${pinyin}\n${chaizi}",
            &[("pinyin", "ni"), ("chaizi", "亻尔")]
        ),
    );
    assert_eq!(split("${a}\\nb", &[("a", "a")]), pair("a", "b"));
}

/// ★ 先拆后 trim：下段变量为空时拼音仍在上方，而不是 `\n` 被尾部 trim 吃掉后掉到右侧。
#[test]
fn split_keeps_pinyin_above_when_lower_part_empty() {
    assert_eq!(
        split(
            "${pinyin}\n${chaizi}",
            &[("pinyin", "ni hao"), ("chaizi", "")]
        ),
        pair("ni hao", "")
    );
    // `\n` 后跟空格再接空变量：空变量只吞那个空格，`\n` 仍在。
    assert_eq!(
        split(
            "${pinyin}\n ${chaizi}",
            &[("pinyin", "ni hao"), ("chaizi", "")]
        ),
        pair("ni hao", "")
    );
}

/// 变量值自带的换行（注释库词条）是内容，不是分隔符。
#[test]
fn newline_inside_variable_value_does_not_split() {
    assert_eq!(split("${dict}", &[("dict", "a\nb")]), pair("", "a\nb"));
}

/// 文档声明的写法约束：`\n` 在可选段内且段为空 ⇒ 随段消失，无上方条。
#[test]
fn newline_inside_empty_optional_group_means_no_above() {
    assert_eq!(
        split(
            "${pinyin}{\n${chaizi}}",
            &[("pinyin", "ni"), ("chaizi", "")]
        ),
        pair("", "ni")
    );
}

/// 可选段非空时，段里的 `\n` 照常拆分（偏移要加上段前已输出的长度）。
#[test]
fn newline_inside_filled_optional_group_splits() {
    assert_eq!(
        split(
            "${pinyin}{\n${chaizi}}",
            &[("pinyin", "ni"), ("chaizi", "亻尔")]
        ),
        pair("ni", "亻尔")
    );
}

#[test]
fn newline_inside_color_splits() {
    assert_eq!(
        split("$[accent]{${p}\n}${q}", &[("p", "ni"), ("q", "亻尔")]),
        pair("ni", "亻尔")
    );
}

#[test]
fn only_first_literal_newline_splits() {
    assert_eq!(split("${a}\nb\nc", &[("a", "a")]), pair("a", "b\nc"));
}

/// 上段为空 ⇒ 视同无上方条，下段照旧。
#[test]
fn empty_upper_part_means_no_above() {
    assert_eq!(
        split(
            "${pinyin}\n${chaizi}",
            &[("pinyin", ""), ("chaizi", "亻尔")]
        ),
        pair("", "亻尔")
    );
}

/// 变量全空 ⇒ 整体消失（隐式可选段），两段都空。
#[test]
fn all_vars_empty_yields_nothing() {
    assert_eq!(
        split(
            "拼${pinyin}\n拆${chaizi}",
            &[("pinyin", ""), ("chaizi", "")]
        ),
        pair("", "")
    );
}

#[test]
fn each_part_truncated_independently() {
    let (a, b) =
        Template::parse("${p}\n${q}").render_split(3, &ev(&[("p", "abcdef"), ("q", "uvwxyz")]));
    assert_eq!((a.into_string(), b.into_string()), pair("abc…", "uvw…"));
}

/// 注释段截断按字素簇计（口径同候选 `truncate_display` 与悬停提示）：
/// ZWJ 序列 / 组合符 / 变体选择符不被拦腰切开，恰好等长不截。整段与拆段两条路径都走。
#[test]
fn truncation_counts_graphemes() {
    let family = "👨\u{200D}👩\u{200D}👧";
    let e = "e\u{301}";
    let ball = "⚽\u{FE0F}";
    let cases = [
        (format!("ab{family}"), format!("ab{family}")),
        (format!("ab{family}c"), format!("ab{family}…")),
        (e.repeat(3), e.repeat(3)),
        (e.repeat(4), format!("{}…", e.repeat(3))),
        (format!("ab{ball}{ball}"), format!("ab{ball}…")),
        ("中a文".to_string(), "中a文".to_string()),
        ("中a文b".to_string(), "中a文…".to_string()),
    ];
    for (input, want) in cases {
        assert_eq!(
            render("${a}", 3, ev(&[("a", &input)])),
            want,
            "整段：{input:?}"
        );
        let (_, b) = Template::parse("${a}").render_split(3, &ev(&[("a", &input)]));
        assert_eq!(b.into_string(), want, "拆段：{input:?}");
    }
}

/// 上段里变量值自带的换行折成空格：上方条恒单行，否则按页等高就被撑破。下段不动。
#[test]
fn newline_in_upper_var_value_becomes_space() {
    assert_eq!(
        split(
            "${dict}\n${chaizi}",
            &[("dict", "a\nb"), ("chaizi", "c\nd")]
        ),
        pair("a b", "c\nd")
    );
    // 样式不丢：替换等长，区间原样。
    let (a, _) = Template::parse("${dict}\n${chaizi}")
        .render_split(0, &ev(&[("dict", "a\nb"), ("chaizi", "")]));
    assert_eq!(a.spans().len(), 1);
    assert_eq!((a.spans()[0].start, a.spans()[0].end), (0, 3));
    assert_eq!(a.spans()[0].role, Some("dict"));
    // 开关关（render_whole）不受影响。
    let s = Template::parse("${dict}\n${chaizi}")
        .render_whole(0, &ev(&[("dict", "a\nb"), ("chaizi", "c")]));
    assert_eq!(s.into_string(), "a\nb\nc");
}

#[test]
fn carriage_return_in_upper_var_value_becomes_space() {
    // DirectWrite 把 `\r` 也当换行：CRLF 词条若留下 `\r`，上方条会变两行、破坏按页等高。
    assert_eq!(
        split("${dict}\n${chaizi}", &[("dict", "a\r\nb"), ("chaizi", "c")]),
        pair("a  b", "c")
    );
}

#[test]
fn no_newline_means_all_below() {
    assert_eq!(split("${pinyin}", &[("pinyin", "ni")]), pair("", "ni"));
}

/// 开关关走 `render_whole`：字面 `\n` 原样留在注释里，不拆。
#[test]
fn switch_off_path_is_unchanged_render_whole() {
    let s = Template::parse("${p}\n${q}").render_whole(0, &ev(&[("p", "a"), ("q", "b")]));
    assert_eq!(s.into_string(), "a\nb");
}

/// 拆分不丢样式：上段的角色与内联色原样带过去。
#[test]
fn split_keeps_span_styles() {
    let (a, b) = Template::parse("$[accent]{${pinyin}}\n${chaizi}")
        .render_split(0, &ev(&[("pinyin", "ni"), ("chaizi", "亻尔")]));
    assert_eq!(a.spans().len(), 1);
    assert_eq!(a.spans()[0].role, Some("pinyin"));
    assert!(a.spans()[0].color.is_some());
    assert_eq!(b.as_str(), "亻尔");
    assert_eq!(b.spans()[0].role, Some("chaizi"));
}
