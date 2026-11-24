use super::legacy_ref;

/// 出厂注释 / 气泡模板、文档与既有用例里出现过的写法，外加宽容解析的退化形态。
const TEMPLATES: &[&str] = &[
    "${code_hint|code_rev|shuangpin}",
    "编码{(${code_source})}",
    "${word_code}",
    "${full_text}",
    "${char}：${readings}",
    "${char}：${chaizi}{ [${chaizi_code}]}",
    "${char}：${unicode}",
    "${char}：{${chaizi}{ [${chaizi_code}]}\t}${readings}",
    "${debug}",
    "(拼: ${pinyin} ${chaizi})",
    "{(拼: ${pinyin} ${chaizi})}",
    "${code_rev}{ (${pinyin})}",
    "${chaizi_all:／} ${chaizi_all: · }",
    "拼:${pinyin}",
    "  ${code_hint}\t",
    "{${a}{ [${b}]}\t}",
    "${a|b|c}",
    "${pinyn} ${a}",
    "${a",
    "{x ${a}",
    "{(${a}{)}",
    "a}b{c",
    "${ a } ${a:} ${a:x:y}",
    "中文标签：${a}，${b}。",
    "$[x]{",
    "",
];

/// 取值夹具：`None` = 未知变量名。`char` 恒有值（气泡逐字段）。
fn fixtures() -> Vec<Vec<(&'static str, &'static str)>> {
    let names = [
        "a",
        "b",
        "c",
        "code_hint",
        "code_rev",
        "shuangpin",
        "code_source",
        "word_code",
        "full_text",
        "readings",
        "chaizi",
        "chaizi_code",
        "unicode",
        "debug",
        "pinyin",
        "chaizi_all",
    ];
    let values = [
        "",
        "x",
        "nǐ hǎo",
        "亻尔",
        " 前后空 ",
        "a\tb",
        "多\n行",
        "😀👨\u{200D}👩",
    ];
    let mut out = Vec::new();
    // 全空、全填同一个值、以及按下标交错取值（空与非空交错，覆盖回退链与吞空白）。
    for v in values {
        out.push(names.iter().map(|n| (*n, v)).collect());
    }
    for shift in 0..values.len() {
        out.push(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (*n, values[(i + shift) % values.len()]))
                .collect(),
        );
    }
    out
}

fn eval<'a>(
    pairs: &'a [(&'static str, &'static str)],
) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
    move |n, _| {
        if n == "char" {
            return Some("好".to_string());
        }
        pairs
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.to_string())
    }
}

/// 参照的截断按码位计，现行按字素簇计（见 `truncate_graphemes`）——这是**有意**的行为
/// 变化，参照本身不改（它的价值在于不变）。故对拍取参照的**不截断**产物，截断这一步按
/// 现行口径在此显式套上：截断之前的渲染仍须与参照逐字节相同。
fn legacy_render_truncated(
    tpl: &str,
    max: usize,
    e: &impl Fn(&str, Option<&str>) -> Option<String>,
) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let s = legacy_ref::render(tpl, 0, e);
    match s.grapheme_indices(true).nth(max) {
        Some((cut, _)) if max > 0 => format!("{}…", &s[..cut]),
        _ => s,
    }
}

#[test]
fn render_matches_pre_change_engine() {
    for tpl in TEMPLATES {
        for fx in fixtures() {
            let e = eval(&fx);
            for max in [0usize, 1, 3, 8] {
                assert_eq!(
                    super::render(tpl, max, &e),
                    legacy_render_truncated(tpl, max, &e),
                    "render({tpl:?}, {max}) 与改动前不同，取值 {fx:?}"
                );
            }
            for counts in [|_: &str| true, |n: &str| n != "char"] {
                assert_eq!(
                    super::Template::parse(tpl).render(&e, &counts),
                    legacy_ref::render_template(tpl, &e, &counts),
                    "Template::render({tpl:?}) 与改动前不同，取值 {fx:?}"
                );
            }
        }
    }
}
