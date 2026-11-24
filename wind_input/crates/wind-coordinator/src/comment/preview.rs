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

/// 样例语种：设置端按作用范围选（英文类型、临时英文模式用英文样例，其余中文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleLang {
    Zh,
    En,
}

impl SampleLang {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }
}

/// 样例候选里一个字的逐字数据（逐字段 `han` / `char` 用）。
#[derive(Debug)]
pub struct SampleChar {
    pub ch: char,
    pub readings: &'static [&'static str],
    pub chaizi: &'static str,
    pub chaizi_code: &'static str,
}

/// 一个样例候选与它的全部变量值。中文专属变量在英文样例里为空，可选段随之消失。
#[derive(Debug)]
pub struct PreviewSample {
    pub id: &'static str,
    pub text: &'static str,
    pub lang: SampleLang,
    pub pinyin: &'static str,
    pub correct_pinyin: &'static str,
    pub shuangpin: &'static str,
    pub code_hint: &'static str,
    pub emoji: &'static str,
    pub code_rev: &'static str,
    pub code_rev_all: &'static [&'static str],
    /// `${dict_all}` 的各条；裸 `${dict}` 取第一条。
    pub dict: &'static [&'static str],
    /// `${dict:<id>}` 按库 id 取；表里没有的 id 为空（同真实求值查不到）。
    pub dict_by_id: &'static [(&'static str, &'static str)],
    pub word_code: &'static str,
    pub code_source: &'static str,
    pub debug: &'static str,
    pub full_text: &'static str,
    pub chars: &'static [SampleChar],
}

const fn zh_char(
    ch: char,
    readings: &'static [&'static str],
    chaizi: &'static str,
    chaizi_code: &'static str,
) -> SampleChar {
    SampleChar {
        ch,
        readings,
        chaizi,
        chaizi_code,
    }
}

/// 样例表。第一个（「你好」）是缺省样例；码取自五笔86（极点）词库与拆字表，双拼为小鹤。
pub const SAMPLES: &[PreviewSample] = &[
    PreviewSample {
        id: "nihao",
        text: "你好",
        lang: SampleLang::Zh,
        pinyin: "nǐ hǎo",
        correct_pinyin: "ni hao",
        shuangpin: "nihc",
        code_hint: "hao",
        emoji: "👋",
        code_rev: "wqvb",
        code_rev_all: &["wq", "wqvb"],
        dict: &["hello", "问候"],
        dict_by_id: &[("zh_en", "int. hello")],
        word_code: "wqvb",
        code_source: "五笔",
        debug: "来源: 拼音",
        full_text: "你好，世界",
        chars: &[
            zh_char('你', &["nǐ"], "亻尔", "wqiy"),
            zh_char('好', &["hǎo", "hào"], "女子", "vbg"),
        ],
    },
    PreviewSample {
        id: "zhongwen",
        text: "中文",
        lang: SampleLang::Zh,
        pinyin: "zhōng wén",
        correct_pinyin: "zhong wen",
        shuangpin: "vswf",
        code_hint: "wen",
        emoji: "🀄",
        code_rev: "khyy",
        code_rev_all: &["khyy"],
        dict: &["n. Chinese language"],
        dict_by_id: &[("zh_en", "n. Chinese language")],
        word_code: "khyy",
        code_source: "五笔",
        debug: "来源: 拼音",
        full_text: "中文",
        chars: &[
            zh_char('中', &["zhōng", "zhòng"], "口丨", "khk"),
            zh_char('文', &["wén"], "亠乂", "yygy"),
        ],
    },
    PreviewSample {
        id: "shurufa",
        text: "输入法",
        lang: SampleLang::Zh,
        pinyin: "shū rù fǎ",
        correct_pinyin: "shu ru fa",
        shuangpin: "uurufa",
        code_hint: "rufa",
        emoji: "⌨️",
        code_rev: "ltif",
        code_rev_all: &["ltif"],
        dict: &["n. input method"],
        dict_by_id: &[("zh_en", "n. input method")],
        word_code: "ltif",
        code_source: "五笔",
        debug: "来源: 拼音",
        full_text: "输入法",
        chars: &[
            zh_char('输', &["shū"], "车俞", "lwgj"),
            zh_char('入', &["rù"], "丿乀", "tyi"),
            zh_char('法', &["fǎ"], "氵去", "ifcy"),
        ],
    },
    en_sample("apple", "🍎", &["n. 苹果"], &[("en_zh", "n. 苹果")]),
    en_sample(
        "hello",
        "👋",
        &["int. 喂；你好"],
        &[("en_zh", "int. 喂；你好")],
    ),
    en_sample("world", "🌍", &["n. 世界"], &[("en_zh", "n. 世界")]),
];

/// 英文样例：只有释义（`en_zh`）、emoji 与来源，拼音 / 码 / 拆字等中文变量全空。
const fn en_sample(
    text: &'static str,
    emoji: &'static str,
    dict: &'static [&'static str],
    dict_by_id: &'static [(&'static str, &'static str)],
) -> PreviewSample {
    PreviewSample {
        id: text,
        text,
        lang: SampleLang::En,
        pinyin: "",
        correct_pinyin: "",
        shuangpin: "",
        code_hint: "",
        emoji,
        code_rev: "",
        code_rev_all: &[],
        dict,
        dict_by_id,
        word_code: "",
        code_source: "英文",
        debug: "来源: 英文",
        full_text: text,
        chars: &[],
    }
}

/// 按 id 取样例；`None` = 缺省样例（「你好」）。
pub fn find_sample(id: Option<&str>) -> Option<&'static PreviewSample> {
    match id {
        None => SAMPLES.first(),
        Some(id) => SAMPLES.iter().find(|s| s.id == id),
    }
}

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
    !wind_ui_types::STRUCTURAL_ROLES.contains(&name)
        && wind_ui_types::TEXT_ROLES.contains(&role_of(name))
}

/// 样例值。`ch` = 逐字段当前字（整段求值为 `None`）。`None` 返回值 = 未知变量名。
fn sample_value(
    s: &PreviewSample,
    name: &str,
    arg: Option<&str>,
    ch: Option<char>,
) -> Option<VarValue> {
    // `_all` 类变量的分隔符参数：样例里用空格连接，参数换掉它。分隔符带 `separator` 标记，同真实求值。
    let joined = |parts: &mut dyn Iterator<Item = &str>, default: &str| {
        Some(VarValue::joined(parts, arg.unwrap_or(default)))
    };
    let this = ch.and_then(|c| s.chars.iter().find(|x| x.ch == c));
    Some(VarValue::from(match role_of(name) {
        "char" => ch.map(String::from).unwrap_or_default(),
        "readings" => {
            let all = this.map_or(&[][..], |x| x.readings);
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
                s.text.chars().map(|c| format!("U+{:04X}", c as u32)),
                arg.unwrap_or(" "),
            ));
        }
        "code_hint" => s.code_hint.into(),
        "emoji" => s.emoji.into(),
        "code_rev" => s.code_rev.into(),
        // 全部码位默认以 `/` 连接（同 `word_codes_in`），参数换掉它。
        "code_rev_all" => return joined(&mut s.code_rev_all.iter().copied(), "/"),
        "shuangpin" => s.shuangpin.into(),
        "pinyin" => s.pinyin.into(),
        "correct_pinyin" => s.correct_pinyin.into(),
        // 逐字段取当前字；整段（注释）取首字，同旧样例。
        "chaizi" => this.or(s.chars.first()).map_or("", |x| x.chaizi).into(),
        "chaizi_code" => this
            .or(s.chars.first())
            .map_or("", |x| x.chaizi_code)
            .into(),
        "chaizi_all" => return joined(&mut s.chars.iter().map(|x| x.chaizi), " "),
        "chaizi_code_all" => return joined(&mut s.chars.iter().map(|x| x.chaizi_code), " "),
        // `dict_all` 归一到 `dict` 角色（同一份数据），但取值不同：按原名分开。
        "dict" if name == "dict_all" => {
            return joined(&mut s.dict.iter().copied(), super::DICT_ALL_SEP);
        }
        // `${dict:<id>}` 只看那个库，裸 `${dict}` 取首条释义。
        "dict" => match arg {
            Some(id) => s
                .dict_by_id
                .iter()
                .find(|(k, _)| *k == id.trim())
                .map_or("", |(_, v)| v)
                .into(),
            None => s.dict.first().copied().unwrap_or_default().into(),
        },
        "word_code" => s.word_code.into(),
        "code_source" => s.code_source.into(),
        "debug" => s.debug.into(),
        "full_text" => s.full_text.into(),
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

/// 按场景用缺省样例（「你好」）求值并诊断。
pub fn sample(template: &str, scene: &TemplateScene) -> TemplateSample {
    sample_with(template, scene, &SAMPLES[0])
}

/// 按场景用指定样例求值并诊断。
pub fn sample_with(template: &str, scene: &TemplateScene, s: &PreviewSample) -> TemplateSample {
    let t = Template::parse(template);
    let layer = Layer::of(scene);
    // 场景外的变量与真实渲染一样返回 `None`（原样回显），不给样例值。
    let value = move |name: &str, arg: Option<&str>, ch: Option<char>| {
        if layer.has(name) {
            sample_value(s, name, arg, ch)
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
                let rows: Vec<StyledText> = s
                    .text
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
#[path = "preview_tests.rs"]
mod tests;
