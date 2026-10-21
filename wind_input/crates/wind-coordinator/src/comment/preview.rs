//! 模板样例求值——设置页「预览行」的数据来源（设计 text-span-colors.md §11）。
//!
//! 设置页编辑注释模板与悬停提示段时，在输入框下方显示当前主题下的渲染效果。这里负责两件
//! 与主题无关的事：
//!
//! 1. 用**固定样例**（候选「你好」，候选专属变量给样例值）按真实的模板引擎渲染出带片段样式的
//!    文字。取样例值而不是现查词库：预览要稳定、可预期，不该随「反查索引建没建好」「这个用户装
//!    没装拆字库」而时有时无——用户在这里看的是**模板写得对不对**，不是词库里有什么。
//! 2. 诊断：未知变量名、内联色的写法与未知 `key=`。都给**模板里的字节区间**（`problems` 的坐标系
//!    是入参模板，不是输出文字；设置端要把它映射到转义后的显示文本上再标注）。
//!
//! 颜色（按当前主题求色、颜色名在当前主题里查不查得到）由 wind-webdata 做——它能加载主题，
//! 这里只交出每个内联色的写法与位置。

use super::{
    MAX_DEPTH, Pairs, Template, VarRef, VarValue, arg_color, color_bounds, role_of, utf8_len,
    var_ref_ranges,
};
use std::sync::Arc;
use wind_theme::InlineColor;
use wind_ui_types::StyledText;

/// 样例候选。两个字都是 ≥ U+3400 的汉字，逐字段（`han` / `char`）各出一行。
pub const SAMPLE_TEXT: &str = "你好";

/// 模板用在哪里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateScene {
    /// 候选注释：整个模板是一个隐式可选段，trim。
    Comment,
    /// 悬停提示段名：字面文字的角色是 `title`，trim。
    TooltipLabel,
    /// 悬停提示段内容：`each` = `""` / `"han"` / `"char"`，逐字段每字一行。
    TooltipContent { each: String },
}

/// 模板里的一处问题：`[start, end)` 是**入参模板**的字节区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateProblem {
    pub start: usize,
    pub end: usize,
    pub message: String,
}

/// 模板里的一个内联色：`[start, end)` 是 `SPEC`（`$[` 与 `]` 之间）在入参模板里的字节区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorSpec {
    pub start: usize,
    pub end: usize,
    pub color: Arc<InlineColor>,
    /// `SPEC` 里被引擎**忽略**的部分（与主题无关）：认不出的 `key=`、没写 `=` 的 `selected`、
    /// 多余的逗号。引擎的处理见 `InlineColor::parse`，这里只把它们说出来。
    pub notes: Vec<String>,
}

/// 样例求值结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSample {
    /// 渲染出的文字（逐字段多行以 `\n` 连接）与片段样式。
    pub text: StyledText,
    /// 与主题无关的问题（未知变量名）。
    pub problems: Vec<TemplateProblem>,
    /// 全部内联色的写法与位置，颜色相关的诊断由调用方按主题做。
    pub colors: Vec<ColorSpec>,
}

/// 求值层级：决定哪些变量有值（与真实渲染的求值链一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// 注释段：只有 `eval_var`。
    Comment,
    /// 气泡段名、整段内容：再加候选级的 `cand_eval` 两层。
    Whole,
    /// 气泡逐字段：再加逐字的 `char_var` / `eval_text_var` / `reverse_text_var`。
    PerChar,
}

impl Layer {
    fn of(scene: &TemplateScene) -> Self {
        match scene {
            TemplateScene::Comment => Self::Comment,
            TemplateScene::TooltipLabel => Self::Whole,
            TemplateScene::TooltipContent { each } => match each.trim() {
                "han" | "char" => Self::PerChar,
                _ => Self::Whole,
            },
        }
    }

    /// 这一层的真实求值链认不认得 `name`。表取自各入口旁的常量（测试逐表对过 match 分支）。
    fn has(self, name: &str) -> bool {
        use super::{EVAL_TEXT_VAR_NAMES, EVAL_VAR_NAMES, REVERSE_TEXT_VAR_NAMES};
        use crate::tooltip::{CHAR_VAR_NAMES, TOOLTIP_CAND_VAR_NAMES, TOOLTIP_COORD_VAR_NAMES};
        let any = |tables: &[&[&str]]| tables.iter().any(|t| t.contains(&name));
        any(&[EVAL_VAR_NAMES])
            || (self != Self::Comment && any(&[TOOLTIP_CAND_VAR_NAMES, TOOLTIP_COORD_VAR_NAMES]))
            || (self == Self::PerChar
                && any(&[CHAR_VAR_NAMES, EVAL_TEXT_VAR_NAMES, REVERSE_TEXT_VAR_NAMES]))
    }

    /// 「此处不可用」的说明。
    fn unavailable_hint(self) -> &'static str {
        match self {
            Self::Comment => "候选注释里没有这个变量，会原样显示",
            Self::Whole => "整段求值时没有这个变量（逐字段才有），会原样显示",
            Self::PerChar => "这里没有这个变量，会原样显示",
        }
    }
}

/// `SPEC` 里被引擎忽略的部分，逐条说明（与 `InlineColor::parse` 的处理一一对应）。
fn spec_notes(spec: &str) -> Vec<String> {
    spec.split(',')
        .skip(1)
        .filter_map(|item| {
            let t = item.trim();
            if t.is_empty() {
                return Some("多余的逗号，已忽略".to_string());
            }
            let key = t.split_once('=').map(|(k, _)| k.trim());
            match key {
                Some("selected") => None,
                None if t == "selected" => {
                    Some("「selected」后面要写「=颜色」，已忽略".to_string())
                }
                _ => Some(format!(
                    "不认识的「{}」，已忽略（目前只支持 selected=）",
                    key.unwrap_or(t)
                )),
            }
        })
        .collect()
}

/// 求值入口认得的变量：契约清单里的变量 + 兼容别名。
fn is_known(name: &str) -> bool {
    !matches!(name, "title" | "literal" | "separator")
        && wind_ui_types::TEXT_ROLES.contains(&role_of(name))
}

/// 样例值。`ch` = 逐字段当前字（整段求值为 `None`）。`None` 返回值 = 未知变量名。
fn sample_value(name: &str, arg: Option<&str>, ch: Option<char>) -> Option<VarValue> {
    // `_all` 类变量的分隔符参数：样例里用空格连接，参数换掉它。分隔符带 `separator` 标记，同真实求值。
    let joined =
        |parts: &[&str], default: &str| Some(VarValue::joined(parts, arg.unwrap_or(default)));
    Some(VarValue::from(match role_of(name) {
        "char" => ch.map(String::from).unwrap_or_default(),
        "readings" => {
            let all: &[&str] = match ch {
                Some('你') => &["nǐ"],
                Some('好') => &["hǎo", "hào"],
                _ => &[],
            };
            let n = arg
                .and_then(|a| a.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let take = if n == 0 { all.len() } else { n.min(all.len()) };
            all[..take].join("/")
        }
        "unicode" => ch
            .map(|c| format!("U+{:04X}", c as u32))
            .unwrap_or_default(),
        "unicode_all" => {
            return Some(VarValue::joined(
                SAMPLE_TEXT.chars().map(|c| format!("U+{:04X}", c as u32)),
                arg.unwrap_or(" "),
            ));
        }
        "code_hint" => "hao".into(),
        "emoji" => "👋".into(),
        "code_rev" => "wqvb".into(),
        // 全部码位默认以 `/` 连接（同 `word_codes_in`），参数换掉它。
        "code_rev_all" => return joined(&["wq", "wqvb"], "/"),
        "shuangpin" => "nihk".into(),
        "pinyin" => "nǐ hǎo".into(),
        "chaizi" => match ch {
            Some('好') => "女子".into(),
            _ => "亻尔".into(),
        },
        "chaizi_code" => match ch {
            Some('好') => "vbg".into(),
            _ => "wqiy".into(),
        },
        "chaizi_all" => return joined(&["亻尔", "女子"], " "),
        "chaizi_code_all" => return joined(&["wqiy", "vbg"], " "),
        // `dict_all` 归一到 `dict` 角色（同一份数据），但取值不同：按原名分开。
        "dict" if name == "dict_all" => return joined(&["hello", "问候"], super::DICT_ALL_SEP),
        "dict" => "hello".into(),
        "word_code" => "wqvb".into(),
        "code_source" => "五笔".into(),
        "debug" => "来源: 拼音".into(),
        "full_text" => "你好，世界".into(),
        _ => return None,
    }))
}

/// 扫出模板里的变量引用与内联色（位置按入参模板计）。结构判定与模板引擎的 `parse_range`
/// 同一套规则、同一张配对表与同一个嵌套上限，不另写一份配对逻辑。
fn scan(
    tpl: &str,
    pairs: &Pairs,
    (start, end): (usize, usize),
    depth: usize,
    vars: &mut Vec<(usize, usize, Vec<String>)>,
    colors: &mut Vec<ColorSpec>,
) {
    let b = tpl.as_bytes();
    let nest = depth < MAX_DEPTH;
    let mut i = start;
    while i < end {
        if nest
            && b[i] == b'$'
            && i + 1 < end
            && b[i + 1] == b'['
            && let Some((spec_end, body_end)) = color_bounds(b, pairs, i + 2, end)
        {
            let spec = &tpl[i + 2..spec_end];
            colors.push(ColorSpec {
                start: i + 2,
                end: spec_end,
                color: Arc::new(InlineColor::parse(spec)),
                notes: spec_notes(spec),
            });
            scan(
                tpl,
                pairs,
                (spec_end + 2, body_end),
                depth + 1,
                vars,
                colors,
            );
            i = body_end + 1;
            continue;
        }
        if b[i] == b'$' && i + 1 < end && b[i + 1] == b'{' {
            if let Some(close) = pairs.var_end(i + 2, end) {
                let mut names = Vec::new();
                for (s, e) in var_ref_ranges(b, pairs, i + 2, close) {
                    let name = VarRef::parse(tpl, pairs, s, e).name;
                    // 分隔符参数里的内联色同样列出，颜色诊断与模板内联色一致。
                    if let Some(colon) = tpl[s..e].find(':')
                        && let Some(((ss, se), _)) = arg_color(b, pairs, s + colon + 1, e)
                    {
                        let spec = &tpl[ss..se];
                        colors.push(ColorSpec {
                            start: ss,
                            end: se,
                            color: Arc::new(InlineColor::parse(spec)),
                            notes: spec_notes(spec),
                        });
                    }
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
                vars.push((i, close + 1, names));
                i = close + 1;
                continue;
            }
        } else if nest
            && b[i] == b'{'
            && let Some(close) = pairs.group_end(i, end)
        {
            scan(tpl, pairs, (i + 1, close), depth + 1, vars, colors);
            i = close + 1;
            continue;
        }
        i += utf8_len(b[i]);
    }
}

/// 按场景用样例求值并诊断。
pub fn sample(template: &str, scene: &TemplateScene) -> TemplateSample {
    let t = Template::parse(template);
    let layer = Layer::of(scene);
    // 场景外的变量与真实渲染一样返回 `None`（原样回显），不给样例值。
    let value = move |name: &str, arg: Option<&str>, ch: Option<char>| {
        if layer.has(name) {
            sample_value(name, arg, ch)
        } else {
            None
        }
    };
    let whole = |name: &str, arg: Option<&str>| value(name, arg, None);
    let text = match scene {
        TemplateScene::Comment => t.render_whole(0, &whole),
        TemplateScene::TooltipLabel => t.render_styled(&whole, &|_| true, true).0.trim(),
        TemplateScene::TooltipContent { each } => match each.trim() {
            "han" | "char" => {
                let han = each.trim() == "han";
                let rows: Vec<StyledText> = SAMPLE_TEXT
                    .chars()
                    .filter(|c| {
                        if han {
                            *c as u32 >= 0x3400
                        } else {
                            !c.is_whitespace()
                        }
                    })
                    .filter_map(|c| {
                        let eval = |name: &str, arg: Option<&str>| value(name, arg, Some(c));
                        let (row, filled) = t.render_styled(&eval, &|n| n != "char", false);
                        filled.then_some(row)
                    })
                    .collect();
                StyledText::join(&rows, "\n")
            }
            _ => {
                let (row, filled) = t.render_styled(&whole, &|_| true, false);
                if filled { row } else { StyledText::new() }
            }
        },
    };
    let (mut vars, mut colors) = (Vec::new(), Vec::new());
    let pairs = Pairs::new(template.as_bytes());
    scan(
        template,
        &pairs,
        (0, template.len()),
        0,
        &mut vars,
        &mut colors,
    );
    let mut problems = Vec::new();
    for (start, end, names) in vars {
        let (unknown, elsewhere): (Vec<String>, Vec<String>) = names
            .into_iter()
            .filter(|n| !layer.has(n))
            .partition(|n| !is_known(n));
        if !unknown.is_empty() {
            problems.push(TemplateProblem {
                start,
                end,
                message: format!("未知变量：{}", unknown.join("、")),
            });
        }
        if !elsewhere.is_empty() {
            problems.push(TemplateProblem {
                start,
                end,
                message: format!(
                    "此处不可用：{}（{}）",
                    elsewhere.join("、"),
                    layer.unavailable_hint()
                ),
            });
        }
    }
    TemplateSample {
        text,
        problems,
        colors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(t: &StyledText) -> Vec<(&str, Option<&str>, bool)> {
        t.spans()
            .iter()
            .map(|s| {
                (
                    &t.as_str()[s.start as usize..s.end as usize],
                    s.role,
                    s.color.is_some(),
                )
            })
            .collect()
    }

    #[test]
    fn comment_scene_renders_sample_with_roles() {
        let s = sample(
            "${code_hint|code_rev} $[accent]{(${pinyin})}",
            &TemplateScene::Comment,
        );
        assert_eq!(s.text.as_str(), "hao (nǐ hǎo)");
        assert_eq!(
            roles(&s.text),
            vec![
                ("hao", Some("code_hint"), false),
                (" ", Some("literal"), false),
                ("(", Some("literal"), true),
                ("nǐ hǎo", Some("pinyin"), true),
                (")", Some("literal"), true),
            ]
        );
        assert!(s.problems.is_empty());
    }

    /// 预览样例的分隔符同样带标记（设置页预览才看得出 separator 配色）；文字不变。
    #[test]
    fn sample_marks_separators() {
        for (tpl, text, sep) in [
            ("${chaizi_all:／}", "亻尔／女子", "／"),
            ("${chaizi_code_all}", "wqiy vbg", " "),
            ("${code_rev_all}", "wq/wqvb", "/"),
            ("${dict_all:}", "hello问候", ""),
            ("${dict_all}", "hello；问候", "；"),
        ] {
            let s = sample(tpl, &TemplateScene::Comment);
            assert_eq!(s.text.as_str(), text, "{tpl}");
            let got: Vec<&str> = s
                .text
                .spans()
                .iter()
                .filter(|sp| sp.separator)
                .map(|sp| &s.text.as_str()[sp.start as usize..sp.end as usize])
                .collect();
            let want: Vec<&str> = if sep.is_empty() { vec![] } else { vec![sep] };
            assert_eq!(got, want, "{tpl}");
        }
        let s = sample(
            "${unicode_all:,}",
            &TemplateScene::TooltipContent { each: "".into() },
        );
        assert_eq!(s.text.as_str(), "U+4F60,U+597D");
        assert_eq!(s.text.spans().iter().filter(|sp| sp.separator).count(), 1);
    }

    /// 分隔符参数里的内联色：样例照样上色、不报未知变量；颜色写法按模板坐标列出供诊断。
    #[test]
    fn sample_lists_separator_inline_color() {
        let tpl = "${chaizi_all:$[nope,hover=x]{／}}";
        let s = sample(tpl, &TemplateScene::Comment);
        assert_eq!(s.text.as_str(), "亻尔／女子");
        assert!(s.problems.is_empty(), "{:?}", s.problems);
        assert_eq!(s.colors.len(), 1);
        let c = &s.colors[0];
        assert_eq!(&tpl[c.start..c.end], "nope,hover=x");
        assert_eq!(c.notes.len(), 1, "未知 key= 同样提示");
        let sep = s.text.spans().iter().find(|sp| sp.separator).unwrap();
        assert!(sep.color.is_some());
    }

    #[test]
    fn per_char_content_one_row_per_han() {
        let s = sample(
            "${char}：${readings}",
            &TemplateScene::TooltipContent { each: "han".into() },
        );
        assert_eq!(s.text.as_str(), "你：nǐ\n好：hǎo/hào");
        let one = sample(
            "${char}：${readings:1}",
            &TemplateScene::TooltipContent {
                each: "char".into(),
            },
        );
        assert_eq!(one.text.as_str(), "你：nǐ\n好：hǎo");
    }

    #[test]
    fn label_scene_marks_title() {
        let s = sample("编码{(${code_source})}", &TemplateScene::TooltipLabel);
        assert_eq!(s.text.as_str(), "编码(五笔)");
        assert!(s.text.spans().iter().all(|sp| sp.in_title));
    }

    /// 问题区间是**模板**坐标：`${pinyn}` 在模板里的位置，而不是输出里回显的位置。
    #[test]
    fn unknown_variables_are_reported_in_template_offsets() {
        let tpl = "码:{${code_hint}} ${pinyn|dict}";
        let s = sample(tpl, &TemplateScene::Comment);
        assert_eq!(s.problems.len(), 1);
        let p = &s.problems[0];
        assert_eq!(&tpl[p.start..p.end], "${pinyn|dict}");
        assert!(p.message.contains("pinyn") && !p.message.contains("dict"));
        // 兼容别名不算未知。
        assert!(
            sample("${code}${code_all}", &TemplateScene::Comment)
                .problems
                .is_empty()
        );
    }

    /// `${dict_all}` 有样例值，分隔符参数生效（默认 `；`）。
    #[test]
    fn dict_all_sample_uses_separator() {
        let s = sample("${dict_all}", &TemplateScene::Comment);
        assert!(s.problems.is_empty());
        assert_eq!(s.text.as_str(), "hello；问候");
        let s = sample(
            "${dict_all:/}",
            &TemplateScene::TooltipContent { each: "".into() },
        );
        assert_eq!(s.text.as_str(), "hello/问候");
    }

    /// 内联色的位置（SPEC 区间，含嵌套在可选段 / 内联色里的）与未知 key。
    #[test]
    fn color_specs_with_positions_and_notes() {
        let tpl = "{a $[accent,hover=x]{b $[#GG]{c}}}";
        let s = sample(tpl, &TemplateScene::Comment);
        let got: Vec<(&str, Vec<String>)> = s
            .colors
            .iter()
            .map(|c| (&tpl[c.start..c.end], c.notes.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "accent,hover=x",
                    vec!["不认识的「hover」，已忽略（目前只支持 selected=）".to_string()]
                ),
                ("#GG", vec![])
            ]
        );
        // 结构不成立的 `$[` 不算内联色。
        assert!(
            sample("$[accent${code_hint}", &TemplateScene::Comment)
                .colors
                .is_empty()
        );
    }

    /// ★ 变量按场景放行，与真实求值链一致：注释里没有气泡的候选级变量，整段里没有逐字变量。
    /// 场景外的变量原样回显（真实候选栏 / 气泡里就是这样），并报「此处不可用」。
    #[test]
    fn variables_are_scoped_by_scene() {
        let whole = TemplateScene::TooltipContent { each: "".into() };
        let han = TemplateScene::TooltipContent { each: "han".into() };
        let unavailable = |tpl: &str, scene: &TemplateScene| {
            sample(tpl, scene)
                .problems
                .iter()
                .any(|p| p.message.starts_with("此处不可用"))
        };
        // 注释：气泡候选级变量不可用，原样回显。
        let s = sample("${word_code}", &TemplateScene::Comment);
        assert_eq!(s.text.as_str(), "${word_code}");
        assert!(unavailable("${word_code}", &TemplateScene::Comment));
        for v in [
            "${full_text}",
            "${debug}",
            "${readings}",
            "${char}",
            "${unicode}",
        ] {
            assert!(unavailable(v, &TemplateScene::Comment), "{v}");
        }
        // 段名 / 整段：候选级变量可用，逐字变量不可用。
        assert_eq!(
            sample("${word_code}", &TemplateScene::TooltipLabel)
                .text
                .as_str(),
            "wqvb"
        );
        assert!(!unavailable("${code_source}${unicode_all}", &whole));
        assert!(unavailable("${char}", &whole));
        assert!(unavailable("${readings}", &TemplateScene::TooltipLabel));
        // 逐字：全部可用。
        assert!(!unavailable(
            "${char}${readings}${unicode}${word_code}${code_hint}",
            &han
        ));
        // 注释里 eval_var 的变量照常可用。
        assert!(!unavailable(
            "${code_hint}${emoji}${dict}${dict_all}${code}",
            &TemplateScene::Comment
        ));
        // 未知变量仍按「未知」报，不混进「不可用」。
        let s = sample("${pinyn}", &TemplateScene::Comment);
        assert!(s.problems[0].message.starts_with("未知变量"));
        assert_eq!(s.problems.len(), 1);
    }

    /// 引擎忽略的 SPEC 片段逐条说明：没写 `=` 的 selected、多余逗号、未知 key。
    #[test]
    fn spec_notes_follow_engine_parse() {
        assert_eq!(
            spec_notes("accent,selected"),
            vec!["「selected」后面要写「=颜色」，已忽略"]
        );
        assert_eq!(spec_notes("accent,"), vec!["多余的逗号，已忽略"]);
        assert!(spec_notes("accent, selected = on_accent").is_empty());
        // 与引擎一致：没写 `=` 的 selected 不产生选中态颜色。
        assert!(InlineColor::parse("accent,selected").selected.is_none());
    }

    /// 样例表覆盖契约里的全部变量（否则预览会把合法变量报成未知）。
    #[test]
    fn every_listed_role_has_a_sample_value() {
        for r in wind_ui_types::TEXT_ROLES {
            if matches!(*r, "title" | "literal" | "separator") {
                continue;
            }
            assert!(sample_value(r, None, Some('你')).is_some(), "{r}");
        }
    }
}
