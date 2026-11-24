//! 候选注释段（候选右侧的小字）的模板渲染，`[ui.comment]` 模板的唯一消费点。
//!
//! # 模板语法
//!
//! | 写法 | 含义 |
//! |---|---|
//! | `${name}` | 变量替换 |
//! | `${name:arg}` | 带参数的变量（如 `${chaizi_all:／}` 指定逐字分隔符） |
//! | `${name:$[颜色]{分隔符}}` | 分隔符单独上色（参数整个写成内联色时；颜色写法同 `$[颜色]{ … }`） |
//! | `${a\|b\|c}` | 取首个非空的变量 |
//! | `{ … }` | 可选段：段内变量全为空则整段（含字面文本）消失；可嵌套，按层数配对，段内字面 `{` 必须配对 |
//! | `$[颜色]{ … }` | 内联色：只管上色，不是可选段（内部变量照常计入外层的「有值」判定）；颜色写法与回落见 `docs/design/text-span-colors.md` §4 |
//!
//! 另有两条隐含规则：
//! - 空变量吞掉紧邻的一个空白：`{(拼: ${pinyin} ${chaizi})}` 在拆字为空时得 `(拼: nǐ hǎo)`。
//! - 整个模板视为一个隐式可选段：所有变量都为空时输出空串。
//!
//! 可用变量见 [`Coordinator::eval_var`]。不做转义（字面 `$`/`{`/`}` 概率极低）。面向用户的完整
//! 说明在文档站 `settings/appearance/candidate-comment`。
//!
//! 横竖各持一份模板：两种排布的可用横向空间差一个数量级。悬停提示由 `ui.tooltip.*` 独家负责，
//! 注释段不往气泡里溢出。取舍见 `docs/design/candidate-comment-templates.md` 附录。
//!
//! # 模板从哪来：模式 → 方案 → 方案类型 → 全局
//!
//! 模板是命名的（内置在 `system.comments.toml`，用户的在 `[ui.comment]`），分配按
//! 作用范围 × 默认/详细 × 竖/横 逐层找，裁决在 `wind_config::comment_lib::CommentLibrary`。
//! 本模块只给出作用范围（[`mode_key_of`] / [`schema_type_of`]）。设计见
//! `docs/design/candidate-comment-templates.md`。
//!
//! # 为什么装配收在协调器
//!
//! 变量的数据源分属多个 crate（wind-engine、wind-reverse、候选自身），只有协调器同时够得着。
//! 解析/渲染（纯函数 [`parse`] / [`render`]）与变量求值（[`Coordinator::comment_for`]）刻意分开，
//! 前者可用任意求值闭包测出完整语法矩阵，不必构造协调器。

use crate::comment_switch::{SwitchEffect, SwitchOutcome};
use crate::coordinator::State;
use crate::pipeline::ModeKind;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::Config;
use wind_config::comment_lib::SwitchMode;
use wind_ui_types::{InlineColor, SpanStyle, StyledText};

use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// 一次变量引用：名字 + 可选参数（`${chaizi_all:／}` 的 `／`）。
///
/// 参数不 trim、名字才 trim：`${chaizi_all: · }` 里的空格正是用户要的分隔符。
///
/// 参数整个写成 `$[颜色]{分隔符}`（`${chaizi_all:$[text_dim]{／}}`）时，变量收到的参数是
/// `分隔符`，颜色只标在值里的分隔符上（[`VarValue`]），写法与回落同模板内联色。
#[derive(Debug, Clone, PartialEq, Eq)]
struct VarRef {
    name: String,
    arg: Option<String>,
    /// 这个变量取到值时片段带的角色：别名归一后、契约清单里的常驻名（清单外为 `None`）。
    /// 解析时算一次——渲染在每次按键的候选循环里，逐次查表会吃掉 §13.3 的性能预算。
    role: Option<&'static str>,
    /// 参数里的内联色：只给分隔符上色。
    sep_color: Option<Arc<InlineColor>>,
}

impl VarRef {
    /// 解析 `tpl[start..end]`（`name` 或 `name:arg`）。只切**第一个**冒号——分隔符本身可以含冒号。
    fn parse(tpl: &str, pairs: &Pairs, start: usize, end: usize) -> Self {
        let s = &tpl[start..end];
        let (name, arg, sep_color) = match s.split_once(':') {
            Some((n, a)) => match arg_color(tpl.as_bytes(), pairs, start + n.len() + 1, end) {
                Some((spec, body)) => (
                    n.trim(),
                    Some(tpl[body.0..body.1].to_string()),
                    Some(Arc::new(InlineColor::parse(&tpl[spec.0..spec.1]))),
                ),
                None => (n.trim(), Some(a.to_string()), None),
            },
            None => (s.trim(), None, None),
        };
        Self {
            name: name.to_string(),
            arg,
            role: wind_ui_types::static_role(role_of(name)),
            sep_color,
        }
    }
}

/// `${…}` 内部 `tpl[from..close]` 按 `|` 切成各变量引用的区间；参数里 `$[…]{…}` 内的 `|` 不切。
fn var_ref_ranges(b: &[u8], pairs: &Pairs, from: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut seg, mut j) = (from, from);
    while j < close {
        if let Some(body_end) = pairs.arg_color_end(b, j, close) {
            j = body_end + 1;
            continue;
        }
        if b[j] == b'|' {
            out.push((seg, j));
            seg = j + 1;
        }
        j += 1;
    }
    out.push((seg, close));
    out
}

/// 参数 `b[from..end]` 是否**整个**写成 `$[SPEC]{BODY}`：是则返回 `(SPEC 区间, BODY 区间)`。
fn arg_color(
    b: &[u8],
    pairs: &Pairs,
    from: usize,
    end: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let body_end = pairs.arg_color_end(b, from, end)?;
    if body_end + 1 != end {
        return None;
    }
    let spec_end = (from + 2..body_end).find(|&j| b[j] == b']')?;
    Some(((from + 2, spec_end), (spec_end + 2, body_end)))
}

/// 变量的取值：文字 + 其中分项分隔符的字节区间。
///
/// `${chaizi_all:／}` 一类变量把分项拼成一串，分隔符要能单独着色（`separator` 角色），故由变量
/// 自己记下分隔符落在哪——渲染时按 `arg` 切开会误切值内部同样的字符（分隔符是空格时尤甚）。
/// 纯文本出口只读文字，与拼接结果逐字相同。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct VarValue {
    text: String,
    /// 分隔符的 `[start, end)`：升序、不重叠、非空。
    seps: Vec<(usize, usize)>,
}

impl VarValue {
    /// 以 `sep` 连接各分项（同 `[T]::join`，空分项照样占位）。
    pub(crate) fn joined<S: AsRef<str>>(items: impl IntoIterator<Item = S>, sep: &str) -> Self {
        let mut v = Self::default();
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 && !sep.is_empty() {
                let start = v.text.len();
                v.text.push_str(sep);
                v.seps.push((start, v.text.len()));
            }
            v.text.push_str(item.as_ref());
        }
        v
    }
}

impl From<String> for VarValue {
    fn from(text: String) -> Self {
        Self {
            text,
            seps: Vec::new(),
        }
    }
}

impl std::ops::Deref for VarValue {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

/// 模板节点。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    /// 字面文本。
    Text(String),
    /// `${a|b}`：按序取首个非空变量。
    Var(Vec<VarRef>),
    /// `{ … }`：段内变量全空则整段消失。可嵌套：`{${a}{ [${b}]}\t}` 里内段只管 `b`，
    /// 外段在 `a`、`b` 任一非空时保留（悬停提示「拆字 / 拼音」合并段就靠这个表达）。
    Group(Vec<Node>),
    /// `$[颜色]{ … }`：只管上色。内部变量照常计入外层的「有值」判定；全空时它自己**不**消失
    /// ——要「空则消失」就再套一层可选段。这样给任意一段花括号配平的片段包上 `$[…]{}`，
    /// 文字输出逐字节不变（设计 text-span-colors.md §4.3）。
    Color(Arc<InlineColor>, Vec<Node>),
}

/// 解析模板。**不会失败**——未闭合的 `${` / `{` 一律退化为字面文本。
///
/// 宽容而非报错：语法写错时让用户在候选栏里看到自己打的原文，最容易自己改对。
fn parse(tpl: &str) -> Vec<Node> {
    let pairs = Pairs::new(tpl.as_bytes());
    parse_range(tpl, &pairs, 0, tpl.len(), 0)
}

/// 解析 `tpl[start..end]`（下标一律是整个模板里的绝对位置，配对表按整串算一次）。
/// `depth` = 外面已套了几层 `{…}` / `$[…]{…}`，到 [`MAX_DEPTH`] 后花括号按字面处理。
fn parse_range(tpl: &str, pairs: &Pairs, start: usize, end: usize, depth: usize) -> Vec<Node> {
    let b = tpl.as_bytes();
    let nest = depth < MAX_DEPTH;
    let mut nodes = Vec::new();
    let mut text = String::new();
    let mut i = start;
    while i < end {
        // `$[颜色]{…}`：语法不成立即 `$` 按字面输出、从 `[` 继续扫描（§4.4）。
        if nest
            && b[i] == b'$'
            && i + 1 < end
            && b[i + 1] == b'['
            && let Some((spec_end, body_end)) = color_bounds(b, pairs, i + 2, end)
        {
            if !text.is_empty() {
                nodes.push(Node::Text(std::mem::take(&mut text)));
            }
            nodes.push(Node::Color(
                Arc::new(InlineColor::parse(&tpl[i + 2..spec_end])),
                parse_range(tpl, pairs, spec_end + 2, body_end, depth + 1),
            ));
            i = body_end + 1;
            continue;
        }
        // `${a|b}` —— 先于裸 `{` 判定，否则变量的 `{` 会被当成段起点。
        if b[i] == b'$' && i + 1 < end && b[i + 1] == b'{' {
            if let Some(close) = pairs.var_end(i + 2, end) {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                nodes.push(Node::Var(
                    var_ref_ranges(b, pairs, i + 2, close)
                        .into_iter()
                        .map(|(s, e)| VarRef::parse(tpl, pairs, s, e))
                        .filter(|v| !v.name.is_empty())
                        .collect(),
                ));
                i = close + 1;
                continue;
            }
            // 未闭合 → 后面全是字面文本。
        } else if nest
            && b[i] == b'{'
            && let Some(close) = pairs.group_end(i, end)
        {
            if !text.is_empty() {
                nodes.push(Node::Text(std::mem::take(&mut text)));
            }
            // 段内由递归解析处理：配对表已按层数配好，段内文本是平衡的。
            nodes.push(Node::Group(parse_range(
                tpl,
                pairs,
                i + 1,
                close,
                depth + 1,
            )));
            i = close + 1;
            continue;
        }
        // 字面 `\n`（反斜杠加 n）按换行：兼容旧版单行框里手写的 `\n`，与真换行等价。
        if b[i] == b'\\' && i + 1 < end && b[i + 1] == b'n' {
            text.push('\n');
            i += 2;
            continue;
        }
        // 按字符推进，保证切片落在 UTF-8 边界上（模板含中文标签文字）。
        let ch_len = utf8_len(b[i]);
        text.push_str(&tpl[i..(i + ch_len).min(end)]);
        i += ch_len;
    }
    if !text.is_empty() {
        nodes.push(Node::Text(text));
    }
    nodes
}

/// UTF-8 首字节 → 该字符的字节数（非法首字节按 1 处理，与解析的宽容取向一致）。
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// `SPEC` 的长度上限（字节）。超过即不算内联色语法——防止一个落单的 `$[` 把后面整段模板
/// 都吞成颜色说明。
const COLOR_SPEC_MAX: usize = 64;

/// 可选段 / 内联色的嵌套上限。更深的 `{` 与 `$[…]{` 按字面文字处理。
///
/// 解析与渲染都按层递归，不设限的话一个几千层的模板就能把服务的栈打穿。32 远超真实用途
/// （出厂最深 3 层），离栈上限还有两个数量级。超限按字面显示。
pub(crate) const MAX_DEPTH: usize = 32;

/// 整个模板的花括号配对表，一次线性扫描算好；逐次往后重扫是平方级，一串未闭合的 `{` 就能
/// 卡死候选渲染。
///
/// 配对规则与逐次重扫逐字相同（测试 `pairs_match_rescan_reference` 按随机模板对拍）：
/// `${` 跳到其后第一个不在参数内联色 `$[…]{…}` 里的 `}`（[`Self::var_end`]；变量未闭合 ⇒ 其后
/// 不再配对，一切段都闭合不了）；其余 `{` `}` 按层配对。从任一段首重扫与从头扫一遍切出的记号
/// 相同——段首本就是整串扫描里的记号边界。
struct Pairs {
    /// `next_close[i]` = `i` 起（含）第一个 `}` 的位置；没有为 `usize::MAX`。
    next_close: Vec<usize>,
    /// `var_close[i]`（`i` = `${` 之后的首字节）= 变量的收尾 `}`；没有为 `usize::MAX`。
    var_close: Vec<usize>,
    /// `group_close[i]`（`b[i] == '{'` 且不是 `${` 的 `{`）= 配对的 `}`；没有为 `usize::MAX`。
    group_close: Vec<usize>,
}

impl Pairs {
    fn new(b: &[u8]) -> Self {
        let n = b.len();
        let mut next_close = vec![usize::MAX; n + 1];
        for i in (0..n).rev() {
            next_close[i] = if b[i] == b'}' { i } else { next_close[i + 1] };
        }
        let mut pairs = Self {
            next_close,
            var_close: vec![usize::MAX; n + 1],
            group_close: vec![usize::MAX; n],
        };
        let mut stack = Vec::new();
        let mut i = 0;
        while i < n {
            if b[i] == b'$' && i + 1 < n && b[i + 1] == b'{' {
                match pairs.scan_var(b, i + 2) {
                    None => break,
                    Some(c) => {
                        pairs.var_close[i + 2] = c;
                        i = c + 1;
                        continue;
                    }
                }
            }
            match b[i] {
                b'{' => stack.push(i),
                b'}' => {
                    if let Some(open) = stack.pop() {
                        pairs.group_close[open] = i;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        pairs
    }

    /// 从 `from` 起找变量的收尾 `}`：参数里成立的 `$[…]{…}` 整块跳过（它的 `}` 不收尾）。
    /// 每个变量只在建表时扫一次，`arg_color_end` 取 BODY 结尾是查表，整体仍线性。
    fn scan_var(&self, b: &[u8], from: usize) -> Option<usize> {
        let mut j = from;
        while j < b.len() {
            if let Some(body_end) = self.arg_color_end(b, j, b.len()) {
                j = body_end + 1;
                continue;
            }
            if b[j] == b'}' {
                return Some(j);
            }
            j += 1;
        }
        None
    }

    /// `b[i..]` 以参数内联色 `$[SPEC]{BODY}` 开头时，返回 BODY 结束 `}` 的位置（须在 `end` 之前）。
    /// `SPEC` 的成立条件同 [`color_bounds`]；`BODY` 是分隔符文字，不嵌套，到其后第一个 `}`。
    fn arg_color_end(&self, b: &[u8], i: usize, end: usize) -> Option<usize> {
        if !(b[i] == b'$' && i + 1 < end && b[i + 1] == b'[') {
            return None;
        }
        let from = i + 2;
        let limit = (from + COLOR_SPEC_MAX + 1).min(end);
        let close = (from..limit).find(|&j| matches!(b[j], b']' | b'{' | b'}' | b'\n'))?;
        if b[close] != b']' || close + 1 >= end || b[close + 1] != b'{' {
            return None;
        }
        let body_end = *self.next_close.get(close + 2)?;
        (body_end < end).then_some(body_end)
    }

    /// `${` 之后（`from` 指向名字首字节）的收尾 `}`，须在 `end` 之前。
    fn var_end(&self, from: usize, end: usize) -> Option<usize> {
        let c = *self.var_close.get(from)?;
        (c < end).then_some(c)
    }

    /// `open` 处的 `{` 的配对 `}`，须在 `end` 之前。
    fn group_end(&self, open: usize, end: usize) -> Option<usize> {
        let c = *self.group_close.get(open)?;
        (c < end).then_some(c)
    }
}

/// `$[` 之后（`from` 指向 `SPEC` 首字节）：语法成立时返回 `(']' 的位置, BODY 结束 '}' 的位置)`。
///
/// 成立只看两条（§4.4）：`SPEC` 里不含 `{` `}` 换行且不超过 64 字节；`]` 紧跟 `{` 且 `BODY`
/// 按层数配对闭合。`SPEC` 写的是什么不影响成立——颜色非法照样是内联色，只是按正文色显示。
fn color_bounds(b: &[u8], pairs: &Pairs, from: usize, end: usize) -> Option<(usize, usize)> {
    let limit = (from + COLOR_SPEC_MAX + 1).min(end);
    let close = (from..limit).find(|&j| matches!(b[j], b']' | b'{' | b'}' | b'\n'))?;
    if b[close] != b']' || close + 1 >= end || b[close + 1] != b'{' {
        return None;
    }
    // BODY 与可选段同一套配对规则：`SPEC` 里不许出现花括号，故 `$[…]{…}` 贡献的花括号天然配平。
    let body_end = pairs.group_end(close + 1, end)?;
    Some((close, body_end))
}

/// 结构角色（§3.2）：模板字面文字在正文里是 `literal`，在段名里是 `title`。
/// 第三个结构角色 `separator` 不进片段的 `role`，是 `Span::separator` 标记（主题没配时随变量同色）。
const ROLE_LITERAL: &str = "literal";
const ROLE_TITLE: &str = "title";

/// 求值入口认得、却不在契约清单 `TEXT_ROLES` 里的变量：它的文字不产角色，主题给它配的色
/// 静默不生效。每个名字只记一次 warn——这条分支只有清单外的名字走得到，热路径没有额外开销。
/// 源码扫描测试 `every_evaluable_variable_is_a_listed_role` 在编译期守同一件事，这里是运行期兜底。
fn warn_unlisted_role(name: &str) {
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
        std::sync::Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen
        .get_or_insert_with(Default::default)
        .insert(name.to_string())
    {
        tracing::warn!(
            variable = name,
            "模板变量不在 TEXT_ROLES 契约清单里：它的文字不带角色，主题的角色色对它不生效"
        );
    }
}

/// 变量名 → 角色名：只有一张别名表（`code` → `code_rev`、`code_all` → `code_rev_all`，
/// 见 `Coordinator::eval_var` 的兼容别名说明）。主题只需认规范名。
pub(crate) fn role_of(name: &str) -> &str {
    match name {
        "code" => "code_rev",
        "code_all" => "code_rev_all",
        // 同一份注释库数据，只是聚合了全部库：主题按 `dict` 配的色对它同样生效。
        "dict_all" => "dict",
        other => other,
    }
}

/// 片段构建器：输出缓冲 + 颜色栈 + 「是否在段名里」。
///
/// `Color` 节点在当前构建器里原地渲染（压栈 / 弹栈），不另开子构建器，否则「空变量吞掉紧邻
/// 空白」跨不过颜色边界。只有 `Group` 用子构建器：它要先渲染再决定整段要不要。
struct Builder {
    out: StyledText,
    colors: Vec<Arc<InlineColor>>,
    in_title: bool,
    /// 是否记录拆分点（上方注释条开关开时才开，见 [`Template::render_split`]）。
    split_on: bool,
    /// 第一个**字面** `\n`（来自 `Node::Text`）在 `out` 里的字节偏移。变量值里的 `\n` 不记。
    ///
    /// 记下之后不会失效：`pop_whitespace` 只弹空格 / 制表符，弹到 `\n` 就停，拆分点之前的
    /// 内容不会再被改动。
    split_at: Option<usize>,
}

impl Builder {
    fn new(in_title: bool) -> Self {
        Self {
            out: StyledText::new(),
            colors: Vec::new(),
            in_title,
            split_on: false,
            split_at: None,
        }
    }

    /// 开启拆分点记录。
    fn with_split(mut self) -> Self {
        self.split_on = true;
        self
    }

    /// 同颜色栈、同段名语境、同拆分开关的空子构建器（`Group` 用）。
    fn child(&self) -> Self {
        Self {
            out: StyledText::new(),
            colors: self.colors.clone(),
            in_title: self.in_title,
            split_on: self.split_on,
            split_at: None,
        }
    }

    fn push(&mut self, s: &str, role: Option<&'static str>) {
        self.push_styled(s, role, false);
    }

    fn push_styled(&mut self, s: &str, role: Option<&'static str>, separator: bool) {
        let color = self.colors.last().cloned();
        self.push_with(s, role, separator, color);
    }

    fn push_with(
        &mut self,
        s: &str,
        role: Option<&'static str>,
        separator: bool,
        color: Option<Arc<InlineColor>>,
    ) {
        let style = SpanStyle {
            role,
            in_title: self.in_title,
            separator,
            color,
        };
        self.out.push(s, &style);
    }

    /// 变量值：分项带变量角色，分隔符另带 `separator` 标记；`sep_color` = 参数里写的内联色，
    /// 只标在分隔符上（压过外层 `$[…]{}`，同内层内联色压过外层）。
    fn push_value(
        &mut self,
        v: &VarValue,
        role: Option<&'static str>,
        sep_color: Option<&Arc<InlineColor>>,
    ) {
        let mut at = 0;
        for &(start, end) in &v.seps {
            self.push(&v.text[at..start], role);
            let color = sep_color.cloned().or_else(|| self.colors.last().cloned());
            self.push_with(&v.text[start..end], role, true, color);
            at = end;
        }
        self.push(&v.text[at..], role);
    }

    fn push_literal(&mut self, s: &str) {
        let role = if self.in_title {
            ROLE_TITLE
        } else {
            ROLE_LITERAL
        };
        if self.split_on
            && self.split_at.is_none()
            && let Some(i) = s.find('\n')
        {
            self.split_at = Some(self.out.len() + i);
        }
        self.push(s, Some(role));
    }

    /// 吞掉末尾一个空格或制表符。
    fn pop_whitespace(&mut self) {
        self.out.pop_if(|c| c == ' ' || c == '\t');
    }
}

/// 渲染节点序列到构建器，返回「本段里出现过非空（且计数的）变量吗」。
///
/// 后者必须与文本分开返回：一个段可能只渲染出字面装饰字符却没填上任何变量，要整段丢弃。
///
/// `eval` 按变量名求值：`None` = 未知变量名，`Some("")` = 已知但为空。未知变量名原样输出
/// `${name}` 并计作已填充，让拼错的变量名显示出来而不是静默变空；回显用正文色、不带角色。
///
/// `counts(name)` 决定「这个变量填上了」算不算数：悬停提示的逐字段里 `${char}` 恒非空，
/// 若计入，查不到读音的字会留下孤零零的 `好：`。注释段传恒真。
fn render_nodes<V: Into<VarValue>>(
    nodes: &[Node],
    eval: &impl Fn(&str, Option<&str>) -> Option<V>,
    counts: &impl Fn(&str) -> bool,
    b: &mut Builder,
) -> bool {
    let mut any = false;
    for node in nodes {
        match node {
            Node::Text(t) => b.push_literal(t),
            Node::Var(refs) => {
                // 未知名恒排在「首个非空」判定之外单独处理：它不是值，是错误提示。
                // 角色 = **实际取到值的那个变量**（`${code_hint|code_rev}` 取到反查码时是 code_rev）。
                // 第三项 = 取到值的变量；`None` = 未知名回显（无角色、无分隔符色）。
                let mut value: Option<(VarValue, bool, Option<&VarRef>)> = None;
                for r in refs {
                    match eval(&r.name, r.arg.as_deref()).map(Into::into) {
                        None => {
                            value = Some((format!("${{{}}}", r.name).into(), true, None));
                            break;
                        }
                        Some(v) if !v.is_empty() => {
                            if r.role.is_none() {
                                warn_unlisted_role(&r.name);
                            }
                            value = Some((v, counts(&r.name), Some(r)));
                            break;
                        }
                        Some(_) => {} // 已知但空 → 试下一个回退
                    }
                }
                match value {
                    Some((v, counted, r)) => {
                        // 角色取契约清单里的常驻名（清单外的名字不产角色，见 `Span::role`）。
                        b.push_value(
                            &v,
                            r.and_then(|r| r.role),
                            r.and_then(|r| r.sep_color.as_ref()),
                        );
                        any |= counted;
                    }
                    // 空变量吞掉紧邻的一个空白：`(拼: ${pinyin} ${chaizi})` 在拆字为空时
                    // 不留下 `)` 前那个多余空格。只吞一个——吞到底会把用户有意排的版式抹平。
                    None => b.pop_whitespace(),
                }
            }
            Node::Group(inner) => {
                let mut child = b.child();
                if render_nodes(inner, eval, counts, &mut child) {
                    // 段内的拆分点换算到外层偏移；段被丢弃时它随段一起消失（写法约束 §3.2）。
                    if b.split_at.is_none()
                        && let Some(o) = child.split_at
                    {
                        b.split_at = Some(b.out.len() + o);
                    }
                    b.out.append(&child.out);
                    any = true;
                } else {
                    // 整段消失时同样吞掉紧邻空白（`${code}{ (${pinyin})}` → `wq`，非 `wq `）。
                    b.pop_whitespace();
                }
            }
            Node::Color(color, inner) => {
                b.colors.push(color.clone());
                any |= render_nodes(inner, eval, counts, b);
                b.colors.pop();
            }
        }
    }
    any
}

/// 渲染模板为带分段样式的文字。**纯函数**。
///
/// 整个模板按一个隐式可选段处理：所有变量都为空 ⇒ 返回空。否则返回渲染结果（已 trim
/// 首尾空白——模板里为分隔而写的空格，在相邻内容缺席时不该留在两端）。
///
/// `max_chars` = 0 表示不限；超出则按字素簇截断并加 `…`（见 [`truncate_graphemes`]）。
pub(crate) fn render_styled<V: Into<VarValue>>(
    tpl: &str,
    max_chars: usize,
    eval: impl Fn(&str, Option<&str>) -> Option<V>,
) -> StyledText {
    Template::parse(tpl).render_whole(max_chars, &eval)
}

/// 注释段的长度截断：超过 `max_chars` 个**字素簇**就只留前 `max_chars` 簇，再接 `…`
/// （继承被截处前一个字的样式）；0 = 不限。口径同候选的 `truncate_display` 与悬停提示——
/// 按码位切会把 emoji ZWJ 序列、组合符、变体选择符拦腰切开。
fn truncate_graphemes(t: StyledText, max_chars: usize) -> StyledText {
    match t.as_str().grapheme_indices(true).nth(max_chars) {
        Some((cut, _)) if max_chars > 0 => t.cut_with_mark(cut, "…"),
        _ => t,
    }
}

/// [`render_styled`] 的纯文本形态：要上屏的文字（`alt_commit_text` 上屏注释、`reverse_render`
/// 的 cmdbar `dict.rev`），`$[…]{}` 只留 `BODY` 的文字（§4.6）。
pub(crate) fn render<V: Into<VarValue>>(
    tpl: &str,
    max_chars: usize,
    eval: impl Fn(&str, Option<&str>) -> Option<V>,
) -> String {
    render_styled(tpl, max_chars, eval).into_string()
}

/// 预解析的模板：配置快照里存一份，候选循环里只渲染不解析。
///
/// 注释段至今仍是每次 [`render`] 现解析（模板串从三层覆盖里现取，没有一个稳定的快照落点）；
/// 悬停提示的段列表是全局一份、随 `ConfigBundle` 重建，故在那里解析一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Template(Vec<Node>);

impl Template {
    pub(crate) fn parse(tpl: &str) -> Self {
        Self(parse(tpl))
    }

    /// 渲染为 `(文本, 是否有计数变量非空)`。
    ///
    /// 与注释段的 [`render`] 不同，这里既不 trim、也不做「全空则整体消失」，留给调用方：悬停提示
    /// 的原始行要保留缩进与 `\t`，段名字面文字不能消失。
    #[cfg(test)]
    pub(crate) fn render<V: Into<VarValue>>(
        &self,
        eval: &impl Fn(&str, Option<&str>) -> Option<V>,
        counts: &impl Fn(&str) -> bool,
    ) -> (String, bool) {
        let (t, filled) = self.render_styled(eval, counts, false);
        (t.into_string(), filled)
    }

    /// 按注释段口径渲染（见 [`render_styled`]）：整个模板是一个隐式可选段、trim、按字截断。
    /// 预解析的模板在候选循环外解析一次、循环里只渲染。
    pub(crate) fn render_whole<V: Into<VarValue>>(
        &self,
        max_chars: usize,
        eval: &impl Fn(&str, Option<&str>) -> Option<V>,
    ) -> StyledText {
        let mut b = Builder::new(false);
        if !render_nodes(&self.0, eval, &|_| true, &mut b) {
            return StyledText::new();
        }
        truncate_graphemes(b.out.into_trimmed(), max_chars)
    }

    /// 上方注释条开关开时的渲染：按模板**字面文字**里第一个 `\n` 拆成 `(上段, 下段)`。
    ///
    /// 先拆、后各自 trim、再各自截断（`max_chars` 为每段）。先 trim 整体会吃掉尾部 `\n`。
    /// 无字面 `\n`、或上段为空 ⇒ 上段空，下段与 [`Self::render_whole`] 同口径。
    pub(crate) fn render_split<V: Into<VarValue>>(
        &self,
        max_chars: usize,
        eval: &impl Fn(&str, Option<&str>) -> Option<V>,
    ) -> (StyledText, StyledText) {
        let mut b = Builder::new(false).with_split();
        if !render_nodes(&self.0, eval, &|_| true, &mut b) {
            return (StyledText::new(), StyledText::new());
        }
        let part = |t: StyledText| truncate_graphemes(t.into_trimmed(), max_chars);
        let Some(s) = b.split_at else {
            return (StyledText::new(), part(b.out));
        };
        // 上段里剩下的 `\n` 只可能来自变量值（字面的第一个就是拆分点）：折成空格，上方条恒单行，
        // 否则会撑破按页等高。下段不动，行为同开关关。
        let above = part(
            b.out
                .slice(0, s)
                .replace_ascii('\n', ' ')
                .replace_ascii('\r', ' '),
        );
        let below = part(b.out.slice(s + 1, b.out.len()));
        (above, below)
    }

    /// 同 [`Self::render`]，产出带分段样式的文字。`in_title` = 这是段名模板：字面文字的角色
    /// 是 `title` 而非 `literal`，变量片段带 `in_title`（角色未配色时回落 `title`，§3.2）。
    pub(crate) fn render_styled<V: Into<VarValue>>(
        &self,
        eval: &impl Fn(&str, Option<&str>) -> Option<V>,
        counts: &impl Fn(&str) -> bool,
        in_title: bool,
    ) -> (StyledText, bool) {
        let mut b = Builder::new(in_title);
        let filled = render_nodes(&self.0, eval, counts, &mut b);
        (b.out, filled)
    }

    /// 模板的**字面文字**里是否含字符 `c`（不看变量值）。悬停提示据此认出「分列行」：
    /// 模板自己写了 `\t` 的段才是有意分列，变量值里带进来的 `\t` 只是内容。
    pub(crate) fn has_literal(&self, c: char) -> bool {
        fn walk(nodes: &[Node], c: char) -> bool {
            // ⚠️ 必须下钻 `Color`：漏了的话 `$[x]{…\t…}` 不被认为是分列段 → 被折行。
            nodes.iter().any(|n| match n {
                Node::Text(t) => t.contains(c),
                Node::Var(_) => false,
                Node::Group(inner) | Node::Color(_, inner) => walk(inner, c),
            })
        }
        walk(&self.0, c)
    }

    /// 模板里是否引用了变量 `name`（含回退链、可选段内）。供调用方决定要不要预先准备
    /// 代价高的数据（调试上下文、反查索引），没引用就不算。
    pub(crate) fn references(&self, name: &str) -> bool {
        fn walk(nodes: &[Node], name: &str) -> bool {
            // ⚠️ 必须下钻 `Color`：漏了的话 `$[x]{${debug}}` 不被认为引用了 debug →
            // 调试上下文不准备 → 段静默为空。
            nodes.iter().any(|n| match n {
                Node::Text(_) => false,
                Node::Var(refs) => refs.iter().any(|r| r.name == name),
                Node::Group(inner) | Node::Color(_, inner) => walk(inner, name),
            })
        }
        walk(&self.0, name)
    }
}

/// 音节间分隔符。**注音用空格**（rime 注音惯例 `nǐ hǎo`）而非隔音符 `'`：
/// 隔音符是**编码**域的写法（`ni'hao` 是「怎么打」），带声调的注音是**读音**域（「怎么读」），
/// 两者混排会让人以为那串可以照着打。
const SYLLABLE_SEP: &str = " ";

/// 按**音节边界真值**把 `code` 切成音节序列：`nihao` + `0b101` → `["ni", "hao"]`。
///
/// `boundary` 的 bit i 置位 = 第 i **字节**是音节起点（见
/// `wind_dict::binformat::DictEntry::boundary`），真值来自词库源数据 `你好\tni hao` 里的
/// 那个空格。bit 0 是整串起点。
///
/// 超过 64 字节的部分并入最后一个音节（bitmask 装不下）；这个界也防住 `>> 64` 溢出移位。
fn syllables_of(code: &str, boundary: u64) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 1..code.len().min(64) {
        if (boundary >> i) & 1 == 1 {
            out.push(&code[start..i]);
            start = i;
        }
    }
    if start < code.len() {
        out.push(&code[start..]);
    }
    out
}

/// 候选的**带声调注音**。
///
/// 声调只在读音表里，多音字的正确读音只有词条编码知道，故拿编码的音节去筛读音表（见
/// `ReverseLookup::toned_pinyin_of`）。非拼音来源候选按词推断读音，推不出退回逐字最常用读音。
///
/// `boundary == 0` 表示无边界信息而非单音节（单音节是 `0b1`），否则五笔码会被当成拼音音节。
fn pinyin_text(
    c: &Candidate,
    infer: impl FnOnce(&str) -> String,
    lookup: impl FnOnce(&str, Option<&[&str]>) -> String,
) -> String {
    // 路径 A —— 拼音来源候选自带词条真值，**优先于任何推断**：`code` 就是用户实际打出这个
    // 词的音节串、`boundary` 是词库标注的切分，比枚举笛卡尔积回查词典更可靠也更省。
    // ⚠️ `boundary == 0` 是「无边界信息」不是「单音节」（单音节是 `0b1`）。
    if c.source == CandidateSource::Pinyin && !c.code.is_empty() && c.boundary != 0 {
        let syls = syllables_of(&c.code, c.boundary);
        return lookup(&c.text, Some(&syls));
    }
    // 路径 B：码表/短语等非拼音来源按词推断（`EngineManager::word_pinyin_syllables` 取第一个能在
    // 拼音词典里查回该词的读音组合），否则「行长」一类词组逐字取常用读音会全错。
    let inferred = infer(&c.text);
    if inferred.is_empty() {
        // 推断失败（含非汉字、生僻多音字超组合数护栏）→ 交由查表层逐字取最常用读音。
        return lookup(&c.text, None);
    }
    let syls: Vec<&str> = inferred.split(' ').filter(|s| !s.is_empty()).collect();
    lookup(&c.text, Some(&syls))
}

/// 去掉拼音声调符号（`nǐ hǎo` → `ni hao`）。`ü` 保留——它是字母不是声调。
///
/// 与 `wind-reverse` 的 `strip_tone` 口径不同、刻意不合并：本函数产出给人看的文本，那边产出
/// 与输入码比对的键（`ü`→`v`）。改其一时核对另一处。
fn strip_tones(s: &str) -> String {
    s.chars()
        .map(|ch| match ch {
            'ā' | 'á' | 'ǎ' | 'à' => 'a',
            'ē' | 'é' | 'ě' | 'è' => 'e',
            'ī' | 'í' | 'ǐ' | 'ì' => 'i',
            'ō' | 'ó' | 'ǒ' | 'ò' => 'o',
            'ū' | 'ú' | 'ǔ' | 'ù' => 'u',
            'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' => 'ü',
            'ń' | 'ň' | 'ǹ' => 'n',
            'ḿ' => 'm',
            other => other,
        })
        .collect()
}

/// 当前模式在 `[ui.comment.assign.mode]` 里的键。**唯一一处**写死这层对应关系——新增模式只加一行。
///
/// `None` = 该模式没有模式层，直接从方案层找起。`special_schema` 把特殊模式下标换成其方案 id。
///
/// 与 [`crate::layout::intent_for`] 同构（声明式重算），但**刻意不含 `add_word`**：加词面板走
/// `show_add_word_preview` 独立绘制路径，根本不经过注释渲染。生僻字、反查、辅助码没有模式层：
/// 前两者没有宿主方案，辅助码只是筛选，注释来源仍是主流程。
pub(crate) fn mode_key_of(
    cfg: &Config,
    active: Option<ModeKind>,
    special_schema: impl FnOnce(u8) -> Option<String>,
) -> Option<String> {
    match active? {
        ModeKind::Mix(i) => cfg
            .schema
            .mix_modes
            .get(i as usize)
            .filter(|m| !m.id.is_empty())
            .map(|m| format!("mix:{}", m.id)),
        ModeKind::Special(i) => special_schema(i).map(|id| format!("special:{id}")),
        ModeKind::TempPinyin => Some("temp_pinyin".into()),
        ModeKind::TempEnglish => Some("temp_english".into()),
        ModeKind::Url => Some("url".into()),
        ModeKind::Email => Some("email".into()),
        ModeKind::Unicode => Some("unicode".into()),
        ModeKind::RareChar | ModeKind::Reverse | ModeKind::AuxCode => None,
    }
}

/// 方案类型（`[ui.comment.assign.type]` 的键）：引擎类型，拼音配了双拼布局单独成类。
/// 读不到或不认识的类型按码表处理，每个方案告警一次。
pub(crate) fn schema_type_of(
    mgr: &wind_engine::EngineManager,
    schema_id: &str,
) -> wind_config::comment_lib::SchemaType {
    use wind_config::comment_lib::SchemaType;
    match mgr.schema_engine_type(schema_id).as_deref() {
        Some("codetable") => SchemaType::CodeTable,
        Some("pinyin") if mgr.pinyin_is_shuangpin_of(schema_id) => SchemaType::Shuangpin,
        Some("pinyin") => SchemaType::Pinyin,
        Some("mixed") => SchemaType::Mixed,
        Some("english") => SchemaType::English,
        other => {
            static WARNED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
            let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
            if !warned.iter().any(|w| w == schema_id) {
                warned.push(schema_id.to_string());
                tracing::warn!("方案 {schema_id} 的引擎类型 {other:?} 不认识，注释按码表类型解析");
            }
            SchemaType::CodeTable
        }
    }
}

// ── 各求值入口认得的变量名 ──────────────────────────────────────────────────────
//
// 与各入口的 match 分支一一对应，`entry_name_tables_match_their_bodies` 扫源码逐表核对。
// 设置页模板预览按场景取它们的并集判「此处可不可用」，与真实求值链同一口径。

/// [`Coordinator::eval_var`]（注释段；气泡整段 / 段名 / 逐字段的最后一层回落）。
pub(crate) const EVAL_VAR_NAMES: &[&str] = &[
    "code_hint",
    "emoji",
    "code_rev",
    "code",
    "code_rev_all",
    "code_all",
    "shuangpin",
    "pinyin",
    "correct_pinyin",
    "chaizi",
    "chaizi_code",
    "chaizi_all",
    "chaizi_code_all",
    "dict",
    "dict_all",
];

/// [`Coordinator::eval_text_var_ex`]（`eval_text_var` 是它的薄包装）自己的分支（其余交给 [`reverse_text_var`]）。
pub(crate) const EVAL_TEXT_VAR_NAMES: &[&str] = &[
    "code_rev",
    "code",
    "code_rev_all",
    "code_all",
    "pinyin",
    "shuangpin",
    "dict",
    "dict_all",
];

/// `${dict_all}` 不写分隔符时的连接符。
pub(crate) const DICT_ALL_SEP: &str = "；";

/// [`reverse_text_var`]。
pub(crate) const REVERSE_TEXT_VAR_NAMES: &[&str] = &[
    "char",
    "chaizi",
    "chaizi_code",
    "chaizi_all",
    "chaizi_code_all",
];

/// 裸文本变量里**只依赖反查表**的那一部分（`char` 与 `chaizi*` 一族）。
///
/// 从 [`Coordinator::eval_text_var`] 拆出来，让测试不构造协调器也能走到生产同一份求值代码。
pub(crate) fn reverse_text_var(
    name: &str,
    arg: Option<&str>,
    text: &str,
    reverse: &wind_reverse::ReverseLookup,
) -> Option<VarValue> {
    // 判据是 `chars().count()` 而非 `len()`：扩展区汉字走代理对，按字节数会被当成词组。
    let single = text.chars().count() == 1;
    Some(VarValue::from(match name {
        "char" => text.to_string(),
        "chaizi" if single => reverse.radicals_of(text, ""),
        "chaizi" => String::new(),
        "chaizi_code" if single => reverse.chaizi_code_of(text),
        "chaizi_code" => String::new(),
        "chaizi_all" => {
            return Some(VarValue::joined(
                reverse.radicals_each(text),
                arg.unwrap_or(" "),
            ));
        }
        "chaizi_code_all" => {
            return Some(VarValue::joined(
                reverse.codes_each(text),
                arg.unwrap_or(" "),
            ));
        }
        _ => return None,
    }))
}

/// 码长（字符数，未启用扩展库的 `*` 前缀不计）是否 ≥ `min_len`；`min_len == 0` 恒真。
fn code_len_at_least(code: &str, min_len: usize) -> bool {
    code.strip_prefix('*').unwrap_or(code).chars().count() >= min_len
}

/// 码表反查的全部码位（`word_codes_display` 以 `/` 连接）按 `sep` 重新连接，`/` 即分隔符。
fn joined_codes(codes: &str, sep: Option<&str>) -> VarValue {
    if codes.is_empty() {
        return VarValue::default();
    }
    VarValue::joined(codes.split('/'), sep.unwrap_or("/"))
}

/// 按住 Alt 期间的别的键：这一键照常处理完再收起详细态，于是 Alt+数字上屏的是详细注释。
///
/// ⚠️ 声明在 `handle_key_event` 开头，理由同 `SoftKeyboardPushOnDrop`：析构时 `state` 锁已还。
pub(crate) struct HoldOffOnDrop<'a>(pub(crate) &'a crate::coordinator::Coordinator);

impl Drop for HoldOffOnDrop<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        self.0.set_comment_hold(&mut state, false);
    }
}

impl State {
    /// 候选窗关闭 / 被新组合替换：两份翻转一并复位（按住那份防 Alt 松开没送到时卡住）。
    pub(crate) fn clear_comment_flips(&mut self) {
        self.comment_session_flip = false;
        self.comment_hold_flip = false;
    }
}

impl crate::coordinator::Coordinator {
    /// 当前生效的注释模板串（`""` = 不显示）：按 模式 → 方案 → 方案类型 → 全局 解析当前态与排布那一格。
    ///
    /// 方案层取 active，不取 `effective_data_schema`：注释模板是呈现类配置，临英 / 临拼 / mix
    /// 的需求已由模式层表达。（注释**库**的过滤是数据类，另见 `ReverseLookup::comment_of`。）
    ///
    /// 返回 `String`：模板库在锁后，借用活不过本函数。每页候选调一次，分配可忽略。
    pub(crate) fn comment_template_for(
        &self,
        cfg: &Config,
        state: &State,
        vertical: bool,
    ) -> String {
        // 注释总开关在各层之上：关就是关（注释、上方注释条、上屏注释都为空）。
        if !cfg.ui.comment.enabled {
            return String::new();
        }
        let schema = self.engine_mgr.active_schema_id();
        let mode = mode_key_of(cfg, state.active, |i| self.special_schema(i));
        let scope = wind_config::comment_lib::CommentScope {
            mode: mode.as_deref(),
            schema: &schema,
            schema_type: schema_type_of(&self.engine_mgr, &schema),
        };
        self.comment_lib()
            .template_text(&scope, Self::effective_comment_state(cfg, state), vertical)
            .to_string()
    }

    /// 注释当前态：持久态 ⊕ 本次候选窗翻转 ⊕ 按住 Alt 翻转。
    pub(crate) fn effective_comment_state(
        cfg: &Config,
        state: &State,
    ) -> wind_config::comment_lib::CommentState {
        use wind_config::comment_lib::CommentState;
        let persist = cfg.ui.comment.state;
        if state.comment_session_flip == state.comment_hold_flip {
            return persist;
        }
        match persist {
            CommentState::Default => CommentState::Detail,
            CommentState::Detail => CommentState::Default,
        }
    }

    /// 改持久态并写回用户层 `ui.comment.state`（清掉本次翻转）；只换内存配置、不重载，随后重绘。
    pub(crate) fn set_comment_state(
        &self,
        state: &mut State,
        to: wind_config::comment_lib::CommentState,
    ) -> anyhow::Result<()> {
        use wind_config::comment_lib::CommentState;
        let value = match to {
            CommentState::Default => "default",
            CommentState::Detail => "detail",
        };
        Config::set_user_string(&["ui", "comment", "state"], value)?;
        self.refresh_config_in_memory(|c| c.ui.comment.state = to);
        // 设了什么就看到什么：本次窗口的翻转一并作废。
        state.comment_session_flip = false;
        self.notify_ui_update(state);
        Ok(())
    }

    /// 翻转「仅本次候选窗」的注释态。只重绘、不重新查询。
    pub(crate) fn toggle_comment_session(&self, state: &mut State) {
        state.comment_session_flip = !state.comment_session_flip;
        if !state.candidates.is_empty() {
            self.notify_ui_update(state);
        }
    }

    /// 执行一个 `comment_state` 按键动词，返回要弹的状态泡文案（反映执行后的生效态）。
    ///
    /// 写盘失败时内存不变，错误交给调用方（按键路径 warn，命令栏返回给用户）。
    /// 三个消费点（空闲 / 会话表 / 命令栏）共用这一处。
    pub(crate) fn apply_comment_state_action(
        &self,
        state: &mut State,
        action: wind_config::CommentStateAction,
    ) -> anyhow::Result<&'static str> {
        use wind_config::CommentStateAction as A;
        match action {
            A::Toggle => {
                use wind_config::comment_lib::CommentState;
                let to = match self.rt().config.ui.comment.state {
                    CommentState::Default => CommentState::Detail,
                    CommentState::Detail => CommentState::Default,
                };
                self.set_comment_state(state, to)?;
            }
            A::Set(to) => self.set_comment_state(state, to)?,
            A::Session => self.toggle_comment_session(state),
        }
        Ok(Self::comment_state_label(Self::effective_comment_state(
            &self.rt().config,
            state,
        )))
    }

    fn comment_state_label(s: wind_config::comment_lib::CommentState) -> &'static str {
        match s {
            wind_config::comment_lib::CommentState::Default => "注释:默认",
            wind_config::comment_lib::CommentState::Detail => "注释:详细",
        }
    }

    /// 命令栏 `ime.toggle("comment")` / `ime.set("comment", ...)`：`want` 为 `None` 即取反。
    pub(crate) fn cmd_comment_state(
        &self,
        want: Option<wind_config::comment_lib::CommentState>,
    ) -> anyhow::Result<()> {
        use wind_config::CommentStateAction as A;
        let action = want.map_or(A::Toggle, A::Set);
        let label = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.apply_comment_state_action(&mut state, action)?
        };
        self.show_tip(label);
        Ok(())
    }

    /// 失焦 / 切走输入法：Alt / Ctrl 的松开多半送不到了，判定中间态与按住翻转一并复位。
    pub(crate) fn reset_comment_alt(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.alt_track = None;
        state.ctrl_track = None;
        self.set_comment_hold(&mut state, false);
    }

    /// 设「按住切换键」翻转；变了且有候选才重绘。
    pub(crate) fn set_comment_hold(&self, state: &mut State, on: bool) {
        if state.comment_hold_flip == on {
            return;
        }
        state.comment_hold_flip = on;
        if !state.candidates.is_empty() {
            self.notify_ui_update(state);
        }
    }

    /// 当前生效的切换方式：注释关了、或切换键是「无」即 `Off`。
    fn comment_switch_mode(c: &wind_config::comment_lib::UiCommentConfig) -> SwitchMode {
        if c.enabled && c.switch_key != wind_config::comment_lib::SwitchKey::None {
            c.switch_mode
        } else {
            SwitchMode::Off
        }
    }

    /// 执行一次切换判定出的动作。
    fn apply_switch_effect(&self, state: &mut State, effect: SwitchEffect) {
        use wind_config::comment_lib::CommentState;
        match effect {
            SwitchEffect::None => {}
            SwitchEffect::ToggleSession => self.toggle_comment_session(state),
            SwitchEffect::TogglePersist => {
                let to = match self.rt().config.ui.comment.state {
                    CommentState::Default => CommentState::Detail,
                    CommentState::Detail => CommentState::Default,
                };
                if let Err(e) = self.set_comment_state(state, to) {
                    tracing::warn!("切换键切换注释持久态写回失败: {e:#}");
                }
            }
            SwitchEffect::HoldOn => self.set_comment_hold(state, true),
            SwitchEffect::HoldOff => self.set_comment_hold(state, false),
        }
    }

    /// 按住档下的功能键切换键：有候选时按下（含自动连发）显示另一态并吃掉，松开收起。
    ///
    /// 只管 `hold` 档；其余档位走会话按键表（[`wind_config::Config::effective_session_actions`]）。
    /// 松开要平台送来：TSF / Linux 送它们吃掉过按下的那个功能键的松开，macOS 只送 F1。
    /// 带 Ctrl / Alt / Win 的组合不认（那是宿主的快捷键）。
    pub(crate) fn handle_comment_fn_hold(
        &self,
        data: &wind_bridge::handler::KeyEventData,
    ) -> Option<wind_bridge::handler::KeyAction> {
        use wind_bridge::handler::KeyAction;
        use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_ALT, MOD_CTRL, MOD_WIN};
        let rt = self.rt();
        let c = &rt.config.ui.comment;
        if Self::comment_switch_mode(c) != SwitchMode::Hold
            || c.switch_key.function_vk() != Some(data.key_code)
            || data.modifiers & (MOD_CTRL | MOD_ALT | MOD_WIN) != 0
        {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match data.event_type {
            EVENT_KEY_DOWN if !state.candidates.is_empty() => {
                self.set_comment_hold(&mut state, true);
                Some(KeyAction::Consumed)
            }
            EVENT_KEY_UP => {
                let held = state.comment_hold_flip;
                self.set_comment_hold(&mut state, false);
                Some(if held {
                    KeyAction::Consumed
                } else {
                    KeyAction::PassThrough
                })
            }
            _ => None,
        }
    }

    /// 组字中 Alt 的按下 / 松开：按 `ui.comment.switch_mode` 切换注释态，判定见 [`crate::comment_switch`]。
    ///
    /// 平台协议（mac / Linux；Windows 的 TSF 不转发 Alt，走 [`Self::handle_comment_alt_hook`]）：
    /// 只在组字中转发 Alt（`key_code` 0xA4 / 0xA5）的按下与松开，不做单击计时；
    /// `modifiers` 带上这次按住期间按过的 Shift / Ctrl / Win 位（粘滞，按过就一直带到松开）。
    /// 返回 `Consumed` 则平台吃掉这次事件，`PassThrough` 则放行。
    ///
    /// 切换键不是这一侧的 Alt（选了 Ctrl 系、或左右不符）：放行、不建 track，只把正在追踪的
    /// 那次按住记为被打断。
    ///
    /// 裸 `lalt` / `ralt` 在 `key_actions` / `session_actions` 里都绑不上（两张键名表都不收），
    /// 故这里不必给用户绑定让位。
    pub(crate) fn handle_comment_alt(
        &self,
        data: &wind_bridge::handler::KeyEventData,
    ) -> wind_bridge::handler::KeyAction {
        use crate::comment_switch::{carries_other_modifier, on_alt_down, on_alt_up, on_other_key};
        use wind_bridge::handler::KeyAction;
        let rt = self.rt();
        let c = &rt.config.ui.comment;
        let is_down = data.event_type == wind_ipc::protocol::EVENT_KEY_DOWN;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !c.switch_key.matches_vk(data.key_code) {
            if is_down {
                on_other_key(&mut state.alt_track);
            }
            return KeyAction::PassThrough;
        }
        let mode = Self::comment_switch_mode(c);
        let has_candidates = !state.candidates.is_empty();
        let now = std::time::Instant::now();
        let combo = carries_other_modifier(data.key_code, data.modifiers);
        let (outcome, effect) = if is_down {
            let r = on_alt_down(mode, has_candidates, &mut state.alt_track, now);
            if combo {
                on_other_key(&mut state.alt_track);
            }
            r
        } else {
            if combo {
                on_other_key(&mut state.alt_track);
            }
            on_alt_up(mode, has_candidates, &mut state.alt_track, now)
        };
        self.apply_switch_effect(&mut state, effect);
        // 松开就不再按住：别让按住翻转卡住（如按住期间改了档位）。
        if !is_down {
            self.set_comment_hold(&mut state, false);
        }
        match outcome {
            SwitchOutcome::Consumed => KeyAction::Consumed,
            SwitchOutcome::PassThrough => KeyAction::PassThrough,
        }
    }

    /// Windows Alt 钩子投来的事件（在钩子动作消费线程执行），折算见 [`crate::comment_switch::on_hook_alt`]。
    /// 重绘走 `notify_ui_update`，不需要按键上下文。
    pub(crate) fn handle_comment_alt_hook(&self, ev: wind_keys::alt_tap::AltEvent) {
        let mode = Self::comment_switch_mode(&self.rt().config.ui.comment);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let effect = crate::comment_switch::on_hook_alt(mode, !state.candidates.is_empty(), ev);
        tracing::debug!("注释切换键（Alt 钩子）：{ev:?} → {effect:?}");
        self.apply_switch_effect(&mut state, effect);
    }

    /// 钩子要接管的 Alt 侧（`wind_keys::alt_tap::GATE_*`），0 = 不要。只有 Windows 走钩子：
    /// mac / Linux 的 Alt 由平台转发，走 [`Self::handle_comment_alt`]。
    pub(crate) fn comment_alt_gate_mask(&self) -> u32 {
        if !cfg!(windows) {
            return 0;
        }
        let rt = self.rt();
        let c = &rt.config.ui.comment;
        crate::comment_switch::alt_gate_mask(c.switch_key, Self::comment_switch_mode(c))
    }

    /// 启动体检：注释切换键与中英切换键、选词键组、会话绑定撞键 → 告警（形制同
    /// `warn_english_case_cycle_conflict`）。撞键时那些功能优先，切换键只在它们不接时生效。
    pub(crate) fn warn_comment_switch_conflict(&self) {
        let rt = self.rt();
        let c = &rt.config.ui.comment;
        if Self::comment_switch_mode(c) == SwitchMode::Off {
            return;
        }
        let mut owners: Vec<&str> = Vec::new();
        // 功能键走会话按键表：用户在 session_actions 里自己绑了这个键，那条优先。
        if c.switch_key
            .function_key_name()
            .is_some_and(|k| rt.config.keys.session_actions.contains_key(k))
        {
            owners.push("会话绑定（keys.session_actions）");
        }
        for vk in 0xA2..=0xA5u32 {
            if !c.switch_key.matches_vk(vk) {
                continue;
            }
            for e in &rt.compiled_hotkeys.key_up {
                if e.match_hash & 0xFFFF != vk {
                    continue;
                }
                let owner = match e.action.as_str() {
                    "toggle_mode" => "中英切换键（keys.toggle_mode_keys）",
                    wind_config::hotkey::SESSION_ACTION => {
                        "选词键组 / 会话绑定（keys.select_key_groups / session_actions）"
                    }
                    "schema_bound" => "修饰键绑定（keys.key_actions / 方案 [key_actions]）",
                    _ => continue,
                };
                if !owners.contains(&owner) {
                    owners.push(owner);
                }
            }
        }
        if owners.is_empty() {
            return;
        }
        tracing::warn!(
            "注释切换键（ui.comment.switch_key = {:?}）同时配作 {}；那些功能优先，它们接手时本键不切换注释。\
             要用本键切换注释：把切换键换成别的键，或把那些功能改绑到别的键",
            c.switch_key,
            owners.join(" / ")
        );
    }

    /// Ctrl 单击的松开（`key_code` 0xA2 / 0xA3）：切换键是这一侧的 Ctrl 且有候选 → 切换并吃掉；
    /// 否则 `None`，落回原语义。
    ///
    /// 来源两条：平台判过单击送来的松开（没有输入会话时；TSF 按 keyup 白名单转发、修饰位自带 Ctrl，
    /// mac / Linux 无条件送、修饰位为 0），或 [`Self::handle_ctrl_raw`] 判出单击后转成的松开。
    /// keyup 分支里排在选词 / 会话绑定 / CapsLock / 方案修饰键绑定 / 中英切换**之后**，撞键时那些功能优先。
    pub(crate) fn handle_comment_ctrl_up(
        &self,
        data: &wind_bridge::handler::KeyEventData,
    ) -> Option<wind_bridge::handler::KeyAction> {
        use crate::comment_switch::{carries_other_modifier, is_ctrl_vk, on_ctrl_up};
        if !is_ctrl_vk(data.key_code) {
            return None;
        }
        let rt = self.rt();
        let c = &rt.config.ui.comment;
        if !c.switch_key.matches_vk(data.key_code) {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let effect = on_ctrl_up(
            Self::comment_switch_mode(c),
            !state.candidates.is_empty(),
            carries_other_modifier(data.key_code, data.modifiers),
        );
        if effect == SwitchEffect::None {
            return None;
        }
        self.apply_switch_effect(&mut state, effect);
        Some(wind_bridge::handler::KeyAction::Consumed)
    }

    /// 如实上报的 Ctrl 按下 / 松开（`EVENT_MOD_DOWN` / `EVENT_MOD_UP`，`key_code` 0xA2 / 0xA3）。
    ///
    /// 平台协议（三端一致）：按下那一刻有输入会话（组字中或候选窗开着），就把这次按下与松开原样
    /// 送来、不吃 Ctrl，松开带上 `TOGGLE_MOD_NOT_TAP` 表示平台看见过服务端看不见的输入；否则走原
    /// 「平台判单击、只送确认过的松开」。一次按住只走一条路。回包吃不吃平台不看。
    ///
    /// - 按下：建 track（[`crate::comment_switch::on_ctrl_raw_down`]）；按住档且切换键是这一侧 → 立即翻转。
    /// - 松开：按住翻转一律收起；判出单击就构造一个原样的 `EVENT_KEY_UP` 走 keyup 分支，中英切换 /
    ///   会话绑定 / 选词 / 方案修饰键绑定 / 注释单击的优先级与原来一字不差。
    ///
    /// 没人接手的一律回 `Consumed`：`PassThrough` 会让 TSF 冲掉 hold 预览、开挂起的组合，而原来
    /// 这次按键根本不会送到服务端。
    pub(crate) fn handle_ctrl_raw(
        &self,
        data: &wind_bridge::handler::KeyEventData,
    ) -> wind_bridge::handler::KeyAction {
        use crate::comment_switch::{
            carries_other_modifier, ctrl_hold_effect, is_ctrl_vk, on_ctrl_raw_down, on_ctrl_raw_up,
            on_other_key,
        };
        use wind_bridge::handler::KeyAction;
        use wind_ipc::protocol::{EVENT_KEY_UP, EVENT_MOD_DOWN, TOGGLE_MOD_NOT_TAP};
        if !is_ctrl_vk(data.key_code) {
            return KeyAction::Consumed;
        }
        let is_down = data.event_type == EVENT_MOD_DOWN;
        let now = std::time::Instant::now();
        let combo = carries_other_modifier(data.key_code, data.modifiers);
        let tap = {
            let rt = self.rt();
            let c = &rt.config.ui.comment;
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let tap = if is_down {
                // Alt 按住期间按 Ctrl：那次 Alt 不再是单击。
                on_other_key(&mut state.alt_track);
                on_ctrl_raw_down(&mut state.ctrl_track, data.key_code, combo, now);
                false
            } else {
                let not_tap = combo || data.toggles & TOGGLE_MOD_NOT_TAP != 0;
                on_ctrl_raw_up(&mut state.ctrl_track, data.key_code, not_tap, now)
            };
            let effect = ctrl_hold_effect(
                Self::comment_switch_mode(c),
                c.switch_key.matches_vk(data.key_code),
                !state.candidates.is_empty(),
                is_down,
            );
            self.apply_switch_effect(&mut state, effect);
            tap
        };
        tracing::debug!(
            "Ctrl 如实上报：vk=0x{:02X} {} mods=0x{:04X} toggles=0x{:02X} → 单击={tap}",
            data.key_code,
            if is_down { "按下" } else { "松开" },
            data.modifiers,
            data.toggles
        );
        if !tap {
            return KeyAction::Consumed;
        }
        let legacy = wind_bridge::handler::KeyEventData {
            event_type: EVENT_KEY_UP,
            toggles: data.toggles & !TOGGLE_MOD_NOT_TAP,
            ..data.clone()
        };
        match wind_bridge::handler::MessageHandler::handle_key_event(self, &legacy) {
            KeyAction::PassThrough => KeyAction::Consumed,
            act => act,
        }
    }

    /// overlay 反查模式（临时拼音 / 快捷输入）：码表用户借拼音反查编码的场景。
    ///
    /// `${code_source}` 据此视作「间接输入」。名字是旧「强制出码」时代的遗留——现在它
    /// 只判模式。
    pub(crate) fn forces_code_hint(state: &State) -> bool {
        matches!(
            state.active,
            Some(ModeKind::TempPinyin) | Some(ModeKind::Mix(_))
        )
    }

    /// 「上屏注释 / 拼音」（`input.alt_commit`，t138）要上屏的文本；空串＝这条候选没有可上屏的。
    ///
    /// - `Pinyin` / `PinyinPlain`：与注释变量 `${pinyin}` 同一算法（[`pinyin_text`]），后者再去调。
    /// - `Comment`：当前生效的注释模板（与候选窗同一裁决与当前态，排布方向
    ///   同 `notify_ui_update`）渲染出的原文，**不做显示截断**——截断是候选窗的空间预算，
    ///   上屏半截注释没有意义。
    ///
    /// ⚠️ 调用方持 state 锁；本函数自取一次 `self.reverse` 读锁（同 `notify_ui_update`）。
    pub(crate) fn alt_commit_text(
        &self,
        state: &State,
        c: &Candidate,
        kind: wind_config::config::AltCommit,
    ) -> String {
        use wind_config::config::AltCommit;
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        let toned = || {
            pinyin_text(
                c,
                |t| self.engine_mgr.word_pinyin_syllables(t),
                |t, syls| reverse.toned_pinyin_of(t, syls, SYLLABLE_SEP),
            )
        };
        match kind {
            AltCommit::Off => String::new(),
            AltCommit::Pinyin => toned(),
            AltCommit::PinyinPlain => strip_tones(&toned()),
            AltCommit::Comment => {
                let rt = self.rt();
                let vertical = self.desired_orientation(state).vertical;
                let tpl = self.comment_template_for(&rt.config, state, vertical);
                let fallback = self
                    .effective_data_schema(state)
                    .unwrap_or_else(|| self.engine_mgr.active_schema_id());
                let is_mix = matches!(state.active, Some(ModeKind::Mix(_)));
                let dict_schema = self.comment_dict_scope(state, c, is_mix, &fallback);
                render(&tpl, 0, |name, arg| {
                    self.eval_var(name, arg, c, &reverse, &dict_schema)
                })
            }
        }
    }

    /// 渲染该候选的注释段。`vertical` 决定用哪份模板。
    ///
    /// `reverse` 由调用方在候选循环**外**取一次读锁传入——每条候选各取一次锁在满页 9 条
    /// × 每次按键的频率下是不必要的争用。
    /// `dict_schema` = 注释库白名单（`[[ui.comment_dicts]].schemas`）的求值作用域，
    /// 由调用方按 `effective_data_schema` 解析一次传入。
    pub(crate) fn comment_for(
        &self,
        c: &Candidate,
        tpl: &Template,
        max_chars: usize,
        reverse: &wind_reverse::ReverseLookup,
        dict_schema: &str,
    ) -> StyledText {
        tpl.render_whole(max_chars, &|name, arg| {
            self.eval_var(name, arg, c, reverse, dict_schema)
        })
    }

    /// 上方注释条开关开时的 [`Self::comment_for`]：返回 `(上段, 下段)`，见
    /// [`Template::render_split`]。开关关时调用方必须走 `comment_for`（不拆分、字面 `\n` 原样留下）。
    pub(crate) fn comment_parts_for(
        &self,
        c: &Candidate,
        tpl: &Template,
        max_chars: usize,
        reverse: &wind_reverse::ReverseLookup,
        dict_schema: &str,
    ) -> (StyledText, StyledText) {
        tpl.render_split(max_chars, &|name, arg| {
            self.eval_var(name, arg, c, reverse, dict_schema)
        })
    }

    /// **任意文本**的反查渲染 —— cmdbar `dict.rev(text, n, format=…)` 的宿主侧实现。
    ///
    /// 与 [`Self::comment_for`] 共用模板渲染器与变量词汇表（[`Self::eval_var`] /
    /// [`Self::eval_text_var`] 刻意挨着放，免得漂移）。取值对象是一段裸文本而非候选，故
    /// `${code_hint}` 这类候选专属变量不存在。不截断：产物就是上屏文本。
    ///
    /// 调用方不得已持有 `self.reverse` 的读锁：本方法自取一次，std `RwLock` 在有写者排队时同线程
    /// 重入读会死锁。
    #[allow(dead_code)] // 现仅 reverse_render_min 被调用，保留为 min_len=0 的语义入口
    pub(crate) fn reverse_render(&self, text: &str, tpl: &str) -> String {
        self.reverse_render_min(text, tpl, 0)
    }

    /// [`Self::reverse_render`]；`min_len > 0` 时 `${code_rev_all}` / `${code_rev}` 只留码长 ≥ 该值的编码
    /// （`dict.rev(..., min_len=N)`）。0 = 不过滤，与 `reverse_render` 完全一致。
    pub(crate) fn reverse_render_min(&self, text: &str, tpl: &str, min_len: usize) -> String {
        if text.is_empty() {
            return String::new();
        }
        // 运行期格式串不在配置里，`DataNeeds` 看不见：拆字表没装就后台装，本次拆字为空、
        // 下次就有。必须在取读锁**之前**判——装表要取写锁。
        let t = Template::parse(tpl);
        if REVERSE_TEXT_VAR_NAMES
            .iter()
            .any(|n| n.starts_with("chaizi") && t.references(n))
            && !self
                .reverse
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .has_chaizi()
        {
            self.ensure_chaizi_async();
        }
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        // ★ `${char}` 恒有值（它只是把待查文本原样回显），故不能让它算进「查到了」。
        // 否则 `render` 的「变量全空则整体消失」就永远不会触发：剪贴板里是个查不到的
        // 字符（英文、标点、生僻字）时，`${char}: ${code} ${pinyin}` 会渲染成孤零零的
        // `A:` 并当作一条正常候选出现 —— 而正确行为是这条候选压根不该存在。
        let found = std::cell::Cell::new(false);
        let out = render(tpl, 0, |name, arg| {
            let v = self.eval_text_var_ex(name, arg, text, &reverse, true, min_len);
            if name != "char" && v.as_deref().is_some_and(|s| !s.is_empty()) {
                found.set(true);
            }
            v
        });
        if found.get() { out } else { String::new() }
    }

    /// 纯文本（无候选身份）的模板变量求值。`None` = 未知变量名。
    ///
    /// 变量语义与 [`Self::eval_var`] 逐项对齐，仅两点差异：
    /// - `char` —— 本入口独有：注释段不需要它（候选文本已在注释左边），而反查产物要自带被查的字；
    /// - `code_rev` —— 恒取主码表反查，不带注释段那道「仅拼音来源候选」门控：剪贴板文本不是
    ///   用户打出来的，反查正是这里的目的。
    /// - `shuangpin` —— 同义，但只能按词推断读音再编码（没有词条真值 `code`+`boundary`）。
    pub(crate) fn eval_text_var(
        &self,
        name: &str,
        arg: Option<&str>,
        text: &str,
        reverse: &wind_reverse::ReverseLookup,
    ) -> Option<VarValue> {
        self.eval_text_var_ex(name, arg, text, reverse, false, 0)
    }

    /// [`Self::eval_text_var`]；`include_disabled` 让 `code_rev` / `code_rev_all` 补上未启用扩展库的码
    /// （`*` 开头）。只有命令栏的剪贴板反查（`dict.rev`）传 `true`，悬停提示等其余入口一律不带。
    /// `min_len > 0` 时两个码变量只留码长 ≥ 该值的编码（`*` 前缀不计码长），0 = 不过滤。
    pub(crate) fn eval_text_var_ex(
        &self,
        name: &str,
        arg: Option<&str>,
        text: &str,
        reverse: &wind_reverse::ReverseLookup,
        include_disabled: bool,
        min_len: usize,
    ) -> Option<VarValue> {
        Some(VarValue::from(match name {
            // 索引未就绪给空串而不是 `None`（`None` 会被当成未知变量名原样回显）。顺手派后台构建
            // （单飞、不阻塞）：运行期格式串不在配置里，`DataNeeds` 看不见，没有别人会替它建。
            "code_rev" | "code" => {
                let code = match self.engine_mgr.codetable_reverse_hint(text) {
                    // 已启用词库里没有时，补未启用扩展库的码（`*` 开头，取最长）。
                    Some(code) if code.is_empty() && include_disabled => self
                        .engine_mgr
                        .disabled_dict_codes(text)
                        .pop()
                        .unwrap_or_default(),
                    Some(code) => code,
                    None => {
                        self.spawn_index_warm(&self.engine_mgr.primary_codetable_id(), false);
                        String::new()
                    }
                };
                // 最长码都不到 min_len ⇒ 没有满足的码，给空串（同「查不到」）。
                if code_len_at_least(&code, min_len) {
                    code
                } else {
                    String::new()
                }
            }
            // `code_all` —— 全部码位，默认 `/` 连接（`我` → `q/trn/trnt`）。反查默认用它而不是 `code`：
            // 后者恒给最长的全码，而「这个字怎么打」要的是简码。
            "code_rev_all" | "code_all" => {
                let sid = self.engine_mgr.code_source_schema();
                // 空串的理由同上面的 `code_rev`。
                let (codes, ready) = match self.engine_mgr.word_codes_display(&sid, text) {
                    Some(codes) => (codes, true),
                    None => {
                        self.spawn_index_warm(&sid, false); // 理由同上
                        (String::new(), false)
                    }
                };
                // 已启用词库的码之后，追加未启用扩展库的码（`*` 开头）。主索引没就绪时不追加：
                // 只剩 `*` 码会让人误以为这个词只有未启用库里的码；与 `code_rev` 同口径。
                let extra = if include_disabled && ready {
                    self.engine_mgr.disabled_dict_codes(text)
                } else {
                    Vec::new()
                };
                let codes = match (codes.is_empty(), extra.is_empty()) {
                    (_, true) => codes,
                    (true, false) => extra.join("/"),
                    (false, false) => format!("{codes}/{}", extra.join("/")),
                };
                // `word_codes_display` 固定用 `/` 连接，换分隔符只能在这里替。
                if min_len > 0 {
                    let kept: Vec<&str> = codes
                        .split('/')
                        .filter(|c| code_len_at_least(c, min_len))
                        .collect();
                    return Some(joined_codes(&kept.join("/"), arg));
                }
                return Some(joined_codes(&codes, arg));
            }
            "pinyin" => {
                // 与 `pinyin_text` 的路径 B 同构：先让引擎按词推断音节（多音字消歧），
                // 推断不出再退回逐字最常用读音。这里没有路径 A —— 裸文本没有词条 code。
                let inferred = self.engine_mgr.word_pinyin_syllables(text);
                if inferred.is_empty() {
                    reverse.toned_pinyin_of(text, None, SYLLABLE_SEP)
                } else {
                    let syls: Vec<&str> = inferred.split(' ').filter(|s| !s.is_empty()).collect();
                    reverse.toned_pinyin_of(text, Some(&syls), SYLLABLE_SEP)
                }
            }
            // `shuangpin` —— 这段文本的双拼编码：先按词推断读音（`word_pinyin_syllables` 做多音字消歧），
            // 推不出给空串。
            "shuangpin" => {
                let inferred = self.engine_mgr.word_pinyin_syllables(text);
                let syls: Vec<&str> = inferred.split(' ').filter(|s| !s.is_empty()).collect();
                self.engine_mgr
                    .shuangpin_code_of_syllables(&syls)
                    .unwrap_or_default()
            }
            // 作用域取活跃方案：本入口（cmdbar `dict.rev`）是**低频**路径，就地取一次
            // 比给整条求值链加一个参数划算；且它没有候选身份，也就没有临英那种
            // 「数据归 english 桶」的语境可言。
            "dict" => {
                let schema = self.engine_mgr.active_schema_id();
                match arg {
                    Some(id) => reverse.comment_of_id(id, text, None, &schema),
                    None => reverse.comment_of(text, None, &schema),
                }
            }
            "dict_all" => {
                let schema = self.engine_mgr.active_schema_id();
                return Some(VarValue::joined(
                    reverse.comments_of(text, None, &schema),
                    arg.unwrap_or(DICT_ALL_SEP),
                ));
            }
            _ => return reverse_text_var(name, arg, text, reverse),
        }))
    }

    /// `${emoji}` 变量的取值：该词的 emoji（空格分隔，至多 `max_per_word` 个）。
    ///
    /// 只在 `show_as = "comment"` 档返回非空（四档互斥，免得同一个 emoji 既进候选又进注释）。
    /// 功能关、表没加载、没命中一律 `None`，调用点换成空串让模板整段消失。
    fn emoji_comment_of(&self, text: &str) -> Option<String> {
        let (max_per_word, min_chars) = {
            let rt = self.rt();
            let e = &rt.config.input.emoji;
            if !e.enabled || e.show_as != "comment" || e.max_per_word == 0 {
                return None;
            }
            (e.max_per_word, e.min_word_chars)
        };
        if text.chars().count() < min_chars {
            return None;
        }
        let guard = self.emoji_dict.read().unwrap_or_else(|e| e.into_inner());
        let list = guard.as_ref()?.lookup(text)?;
        // 与追加档同一套去重（剥 U+FE0F，保留先到者），否则注释里同一符号出两次。
        let mut seen = std::collections::HashSet::new();
        let picked: Vec<&str> = list
            .split_whitespace()
            .filter(|e| seen.insert(crate::handle_candidate::emoji_dedup_key(e)))
            .take(max_per_word)
            .collect();
        if picked.is_empty() {
            None
        } else {
            Some(picked.join(" "))
        }
    }

    /// 变量求值。`None` = 未知变量名（渲染层据此原样回显 `${name}`）。
    ///
    /// 可用变量：
    /// - `code_hint` —— 引擎产的编码提示（码表前缀候选的剩余编码、混输来源标记 `拼`），取自
    ///   `Candidate::comment`，这是该字段的唯一消费点。
    /// - `code_rev` / `code_rev_all` —— 主码表整词反查编码（单个最长全码 / 全部码位）。仅对拼音来源
    ///   候选生效：码表方案下候选的码就是用户自己打的。
    /// - `shuangpin` —— 候选的双拼编码，由 code+boundary 算出；全拼方案下装了双拼方案照样有值。
    /// - `code` / `code_all` —— `code_rev` / `code_rev_all` 的永久兼容别名，不进文档站：模板是自由
    ///   文本，没有静默迁移路径，删掉别名老用户会看到字面 `${code}`。
    /// - `pinyin` —— 带声调注音，见 [`pinyin_text`]。
    /// - `chaizi` / `chaizi_code` —— 拆字字根串 / 拆字库编码，仅单字候选（词组的字根拼接会把
    ///   候选行撑宽，而候选窗不折行）。
    /// - `chaizi_all[:分隔符]` / `chaizi_code_all[:分隔符]` —— 不限字数的逐字字根 / 编码，默认空格连接。
    /// - `emoji` —— 见 [`Self::emoji_comment_of`]。
    /// - `dict[:库 id]` —— 注释词库（`[[ui.comment_dicts]]`）里该词的注释，键是词，候选 `code` 作可选消歧。
    /// - `dict_all[:分隔符]` —— 全部适用注释库各出一条，去重，默认 `；` 连接。
    ///
    /// `arg` = `${name:arg}` 的冒号后部分（未 trim）。不支持参数的变量静默忽略它，不退化成错误回显。
    pub(crate) fn eval_var(
        &self,
        name: &str,
        arg: Option<&str>,
        c: &Candidate,
        reverse: &wind_reverse::ReverseLookup,
        dict_schema: &str,
    ) -> Option<VarValue> {
        // 判据是 `chars().count()` 而非 `len()`：汉字是多字节，按字节数单字也会被当成词组。
        let single = c.text.chars().count() == 1;
        Some(VarValue::from(match name {
            "code_hint" => c.comment.clone(),
            // 取不到给空串而不是 `?`：`None` 在渲染层是「未知变量名」会原样回显，而这里是「已知变量、
            // 这次为空」，应让所在可选段整段消失。下面 `code_rev` 同理。
            "emoji" => self.emoji_comment_of(&c.text).unwrap_or_default(),
            "code_rev" | "code" => {
                if c.source == CandidateSource::Pinyin {
                    self.engine_mgr
                        .codetable_reverse_hint(&c.text)
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            // 门控与 `code_rev` 一致。与 [`Self::eval_text_var`] 的同名变量同义，两处须一起改。
            "code_rev_all" | "code_all" => {
                if c.source == CandidateSource::Pinyin {
                    let sid = self.engine_mgr.code_source_schema();
                    let codes = self
                        .engine_mgr
                        .word_codes_display(&sid, &c.text)
                        .unwrap_or_default();
                    return Some(joined_codes(&codes, arg));
                } else {
                    String::new()
                }
            }
            // 双拼编码不存在于任何词库里（双拼只是「全拼 + 一张键盘布局」），只能算不能查。布局来源有
            // 回退链（见 `shuangpin_hint_schema`），全拼方案下装了双拼方案照样有值。
            "shuangpin" => {
                if c.source == CandidateSource::Pinyin {
                    self.engine_mgr
                        .shuangpin_code_of(&c.code, c.boundary)
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            "pinyin" => pinyin_text(
                c,
                |t| self.engine_mgr.word_pinyin_syllables(t),
                |t, syls| reverse.toned_pinyin_of(t, syls, SYLLABLE_SEP),
            ),
            // `correct_pinyin` —— 带已勾选来源提示的候选的整词正确拼音（`zhong guo`），带调随
            // `[input.correction.hint] tone`。按来源过滤，不受 `hint.enabled` 与 `hint.scope`
            // 约束；引擎在模板引用它时即使 `hint` 关着也会算提示（`DataNeeds::correct_pinyin`）。
            "correct_pinyin" => {
                let rt = self.rt();
                let hint = &rt.config.input.correction.hint;
                let tone = !c.meta.pinyin_hints.is_empty() && hint.tone;
                crate::hint_text::correct_pinyin_text(c, hint, tone.then_some(reverse))
            }
            "chaizi" if single => reverse.radicals_of(&c.text, ""),
            "chaizi" => String::new(),
            "chaizi_code" if single => reverse.chaizi_code_of(&c.text),
            "chaizi_code" => String::new(),
            "chaizi_all" => {
                return Some(VarValue::joined(
                    reverse.radicals_each(&c.text),
                    arg.unwrap_or(" "),
                ));
            }
            "chaizi_code_all" => {
                return Some(VarValue::joined(
                    reverse.codes_each(&c.text),
                    arg.unwrap_or(" "),
                ));
            }
            // 候选 `code` 作可选消歧（见 `ReverseLookup::comment_of`）。注释库的 `schemas` 白名单按
            // `dict_schema`（`effective_data_schema`）求值：注释库是数据类资源，临英下要按 `english` 查；
            // 这与注释模板取 active 刻意不同。`${dict:id}` 只查那一个库。查询词与大小写回退见 [`dict_query_key`]。
            "dict" => {
                let (key, fb) = dict_query_key(c);
                reverse.comment_of_with(arg, key, Some(&c.code), dict_schema, fb)
            }
            // 全部适用库各出一条，按内容去重。不带来源库标签：标签该写在库内容里。
            "dict_all" => {
                let (key, fb) = dict_query_key(c);
                return Some(VarValue::joined(
                    reverse.comments_of_with(key, Some(&c.code), dict_schema, fb),
                    arg.unwrap_or(DICT_ALL_SEP),
                ));
            }
            _ => return None,
        }))
    }
}

/// 候选查注释库用的词与大小写回退方向。
///
/// | 候选 | 查的词 | 回退 |
/// |---|---|---|
/// | 未改写 | 显示文本 | 双向（输入 `abc` 要能查到缩写 `ABC`） |
/// | 英文**词库**词被改写（投影 / 大小写档） | 词库原文 `case_source` | 只往小写 |
/// | 头部候选（用户所打原文及其变形）被大小写档改写 | **显示文本** | 只往小写 |
///
/// 词库已确证是小写的词，向上回退就是换了个词（输入 `ASP` 时词库的 `asp` 不该命中缩写 `ASP`）。
/// 头部候选的 `case_source` 只是用户打出的形态，按显示文本查；全小写档是用户显式要的小写，同样
/// 只往小写走。
fn dict_query_key(c: &Candidate) -> (&str, wind_reverse::CaseFallback) {
    use wind_reverse::CaseFallback::{Both, LowerOnly};
    match &c.case_source {
        None => (c.text.as_str(), Both),
        Some(orig) if c.source == CandidateSource::English => (orig.as_str(), LowerOnly),
        Some(_) => (c.text.as_str(), LowerOnly),
    }
}

#[cfg(test)]
#[path = "comment_mode_template_tests.rs"]
mod mode_template_tests;

#[cfg(test)]
#[path = "comment_comment_scope_tests.rs"]
mod comment_scope_tests;

#[cfg(test)]
#[path = "comment_tests.rs"]
mod tests;

/// 上方注释条的拆分（`ui.candidate.comment_above`，设计 candidate-comment-above-line.md §3.2）。
///
/// 夹具的 `ev()` 对未列出的变量名返回 `None`（原样回显），每条用例要把引用的变量列全，且至少
/// 含一个非空变量，否则整个模板按隐式可选段消失。
#[cfg(test)]
#[path = "comment_split_tests.rs"]
mod split_tests;

/// `eval_var` 的变量分发与门控。
///
/// 其余测试模块走 mock 求值闭包，只验模板语法；这里验 `eval_var` 本身的分发与门控。
/// 用仓库自带的 `data/` 作数据目录（只需方案定义与双拼布局），不依赖 `build_dev/data`，免得没数据时静默跳过。
#[cfg(test)]
#[path = "comment_eval_var_tests.rs"]
mod eval_var_tests;

/// 设置页预览行的模板样例求值与诊断（设计 text-span-colors.md §11）。
pub mod preview;

// 改动前的模板引擎逐字副本，只作对拍参照（见文件头）。
#[cfg(test)]
#[path = "comment_legacy_ref.rs"]
mod legacy_ref;

/// 纯文本输出与改动前逐字节相同（设计 text-span-colors.md §13.2 P2）：出厂与文档里的模板 ×
/// 各种取值夹具 × 截断上限 × 两种「算不算数」口径，逐一对拍改动前的引擎副本。
#[cfg(test)]
#[path = "comment_legacy_parity_tests.rs"]
mod legacy_parity;

/// 内联色 `$[…]{}` 与片段角色（设计 text-span-colors.md §3.2、§4、§8.1）。
#[cfg(test)]
#[path = "comment_styled_tests.rs"]
mod styled_tests;

/// 分隔符角色：`${name:分隔符}` 的分隔符带 `separator` 标记、角色仍是变量名；纯文本与改前逐字相同。
#[cfg(test)]
#[path = "comment_separator_tests.rs"]
mod separator_tests;

/// 入参防护：嵌套上限与线性配对（`MAX_DEPTH` / `Pairs`）。
#[cfg(test)]
#[path = "comment_bound_tests.rs"]
mod bound_tests;

/// `TEXT_ROLES` 契约清单 ⊇ 全部求值入口认得的变量名（设计 text-span-colors.md §3.1）。
///
/// 清单外的变量不产角色、主题给它配的色静默不生效。求值入口没有可枚举的集合，故扫源码抽出
/// 各 `match` 分支名，经别名表归一后逐个核对。
#[cfg(test)]
#[path = "comment_role_contract_tests.rs"]
mod role_contract_tests;

#[cfg(test)]
#[path = "comment_comment_reverse_regular_tests.rs"]
mod comment_reverse_regular_tests;
