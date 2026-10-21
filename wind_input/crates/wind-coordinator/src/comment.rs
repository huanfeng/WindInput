//! 候选**注释段**（候选右侧的小字）的模板渲染——`ui.candidate.comment_template_*` 的唯一消费点。
//!
//! # 模板语法
//!
//! | 写法 | 含义 |
//! |---|---|
//! | `${name}` | 变量替换 |
//! | `${name:arg}` | 带参数的变量（如 `${chaizi_all:／}` 指定逐字分隔符） |
//! | `${name:$[颜色]{分隔符}}` | 分隔符单独上色（参数**整个**写成内联色时；颜色写法同 `$[颜色]{ … }`） |
//! | `${a\|b\|c}` | 取**首个非空**的变量 |
//! | `{ … }` | 可选段：段内变量**全为空**则整段（含字面文本）消失；可嵌套，段内字面 `{` 必须配对 |
//! | `$[颜色]{ … }` | 内联色：只管上色，**不是**可选段（内部变量照常计入外层的「有值」判定，全空时自己也不消失）；颜色写法与回落见 `docs/design/text-span-colors.md` §4 |
//!
//! ⚠️ 可选段按层数配对（2026-09 起，为悬停提示合并段的 `{${chaizi}{ [${chaizi_code}]}\t}`）。
//! 此前段内第一个 `}` 即结束，段里落单的字面 `{`（如 `{(${a}{)}`）原样输出；现在它会去配
//! 下一个 `}`，输出随之不同。模板里要写字面花括号，请成对出现。
//!
//! 另有两条隐含规则：
//! - **空变量吞掉紧邻的一个空白**：`{(拼: ${pinyin} ${chaizi})}` 在拆字为空时得
//!   `(拼: nǐ hǎo)` 而非 `(拼: nǐ hǎo )`。
//! - **整个模板视为一个隐式可选段**：所有变量都为空时输出空串，故 `拼:${pinyin}` 在查不到
//!   读音时不会剩下一个孤零零的 `拼:`。
//!
//! 可用变量见 [`Coordinator::eval_var`]。不做转义：注释模板里出现字面 `$`/`{`/`}` 的概率极低，
//! 真需要时再加，现在加只是徒增用户要记的规则。
//!
//! 面向用户的完整说明在文档站 `settings/appearance/candidate-comment`（设置页对话框里有跳转按钮）——
//! 语法还会继续长（将来的注释库变量、模式级覆盖），塞进设置页的说明框只会越来越挤。
//!
//! # 为什么是模板而不是「来源列表 + 分隔符」
//!
//! 前一版是有序来源列表 `comment_sources` 加一个 `comment_separator`。它表达不了装饰字符
//! （括号、标签文字）、表达不了「A 为空时用 B」，而且**顺序、内容、分隔三件事散在三个键里**。
//! 模板把它们收进一个字符串，顺带消掉了三处复杂度：分隔符配置、按长度量级硬编码的横排过滤、
//! 以及为保出厂零回归而引入的「编码类来源互斥」。
//!
//! 更要紧的是**可扩展性**：新增一个注释来源（如将来的独立注释库）只是多一个变量名，
//! 配置结构完全不动。
//!
//! # 横竖各持一份模板
//!
//! 两种排布的可用横向空间差一个数量级（竖排每行独占，横排全部候选共享一行宽度），能放什么
//! 本就不是同一个答案。共用一份的结果必是「为竖排配的拼音把横排候选窗撑爆」或「为横排收着
//! 配的注释让竖排一片空白」。
//!
//! **由此，上一版的「溢出转悬停提示」机制已删除**：横排显示什么由横排模板自己决定，
//! 「放不下所以推去气泡」这个前提不存在了。那套机制本身还有个缺陷——它按同名标签追加
//! （`拼音:` / `拆字:`），而 `ui.tooltip.pinyin_enabled` 出厂即 `true`，于是气泡里会同时
//! 出现 tooltip 自己的逐字 `[拼音]` 段和追加的那份，等于把重复做实了。悬停提示现回归由
//! `ui.tooltip.*` 独家负责。
//!
//! # 三层：模式级 → 方案级 → 全局
//!
//! | 层 | 落点 |
//! |---|---|
//! | 模式级 | `input.{temp_english,temp_pinyin,url}` / `schema.mix_modes[]` / 方案文件 `[overlay]` |
//! | 方案级 | 方案文件 `[candidate].comment_template_*` |
//! | 全局 | `ui.candidate.comment_template_*` |
//!
//! 上两层是三态（键缺失=跟随下一层／非空=覆盖／空串=本层不显示注释），最底层必有值。
//! 决策点 [`resolve_template`]，与 `layout::vertical_for` 同构。
//!
//! ★ **「没意见」的语义是「跟随下一层」而不是「跟随全局」**——加了方案层之后这两者
//! 不再等价。设计与判据见 `docs/design/candidate-comment-layering.md`。
//!
//! # 为什么装配收在协调器
//!
//! 变量的数据源分属多个 crate —— `Candidate::comment`（wind-engine 产）、候选自身的
//! `code`/`boundary`、`ReverseLookup`（wind-reverse）、`codetable_reverse_hint`
//! （wind-engine）。只有协调器同时够得着。
//!
//! 解析/渲染（纯函数 [`parse`] / [`render`]）与变量求值（[`Coordinator::comment_for`]）
//! 刻意分开，与 `layout.rs` 同构：前者可用任意求值闭包测出完整语法矩阵，不必构造协调器。

use crate::coordinator::State;
use crate::pipeline::ModeKind;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::config::{CodeHintSource, CommentTemplateOverride};
use wind_config::{Config, OverlaySpec};
use wind_ui_types::{InlineColor, SpanStyle, StyledText};

use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// 一次变量引用：名字 + 可选参数（`${chaizi_all:／}` 的 `／`）。
///
/// 参数**不 trim**，名字才 trim：`${chaizi_all: · }` 里那两个空格正是用户要的分隔符，
/// 削掉它就没法配出「亻尔 · 女子」。而 `${ pinyin }` 这种手滑仍要认。
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
/// 宽容而非报错，是因为这个字符串由用户在设置页手打，且它的产物直接显示在候选栏里：
/// 语法写错时让他看到自己打的原文（`${pinyn}` 原样出现），比弹一个错误对话框或者静默
/// 变空更容易自己改对。
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
/// 解析与渲染都按层递归，层数不设限的话，一个几千层的模板（手滑粘贴、或故意构造）存进
/// 配置就能在候选渲染时把服务的栈打穿、进程直接 abort。32 远超任何真实用途：出厂最深的
/// 是悬停提示合并段的 3 层，手写模板很难超过 5 层；而 32 层 × 每层几百字节的栈帧离线程栈
/// 上限还有两个数量级。超限不报错、按字面显示，与本解析器「写错就原样给人看」的取向一致。
pub(crate) const MAX_DEPTH: usize = 32;

/// 整个模板的花括号配对表，**一次线性扫描**算好。
///
/// 此前 `find_group_end` 每遇到一个 `{` 就往后重扫到配对处，`${` 找 `}` 也是逐次往后找：
/// 一串未闭合的 `{`（20 万个）就是平方级，实测 37 秒——同样是存进配置就卡死候选渲染的形状。
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
/// `Color` 节点**在当前构建器里原地渲染**（进入时压栈、退出时弹栈），不另开子构建器：
/// 「空变量吞掉紧邻的一个空白」看的永远是同一个输出缓冲的末尾，跨颜色边界照吞。若 `Color`
/// 也开子构建器，`${pinyin} $[accent]{${chaizi}}` 里 `${chaizi}` 为空时它看到的是空的子缓冲，
/// 吞不到外面那个空格（§8.1）。只有 `Group` 用子构建器——它要先渲染再决定整段要不要。
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
/// 后者是可选段与顶层的存废依据，**必须与文本分开返回**：一个段可能渲染出非空文本
/// （字面装饰字符）却一个变量都没填上，那正是要整段丢弃的情形（`(拼: )`）。
///
/// `eval` 按变量名求值：`None` = **未知变量名**，`Some("")` = 已知但为空。
///
/// 两者刻意区分：未知变量名原样输出 `${name}` 并**计作已填充**，于是拼错的变量名一定会
/// 显示在候选栏里让用户看见。若把未知当空处理，用户得到的是「配了没反应」——本仓记忆里
/// 反复出现的那类静默失效。回显不带角色：它是错误提示，用正文色，不借任何角色的色。
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
    /// 与注释段的 [`render`] 不同，这里**既不 trim、也不做「全空则整体消失」**，都留给调用方：
    /// 悬停提示的原始行是复制 / 上屏的取值来源，完整原文开头的缩进、`\t` 必须原样保留；
    /// 段内容全空要消失，而段名的字面文字（`编码`）不能消失。
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
    /// ★ 先拆、后各自 trim、再各自截断（`max_chars` 含义为「每段」）。先 trim 整体会让
    /// `${pinyin}\n${chaizi}` 在拆字为空时吃掉尾部 `\n`，拼音就掉到了右侧。
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
/// 超过 64 字节的部分不再有边界信息（bitmask 装不下），并入最后一个音节 —— 拼音词长上限
/// 远小于此，实际不触发，但不加这个界会读到 `>> 64` 的未定义移位。
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
/// 声调只存在于读音表（`pinyin_map.txt`）里，编码域一律无调（拼音输入法不打声调）；而多音字
/// 的正确读音只有词条编码知道。故两边都要用：**拿编码的音节去筛读音表**，见
/// `ReverseLookup::toned_pinyin_of`。
///
/// 拼音来源候选带 `code` + `boundary` 时给得出音节序列（消歧准确）；其余候选（码表/短语/
/// 英文…）没有拼音码可依，传 `None` 退回逐字最常用读音，多音字可能不准 —— 这是数据下界。
///
/// ⚠️ `boundary == 0` 的语义是「**无边界信息**」而非「单音节」（单音节是 `0b1`），必须当作
/// 拿不到音节序列。否则五笔码 `wqvb` 会被当成一个拼音音节送去筛读音表。
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
    // 路径 B —— 码表/短语等非拼音来源：没有拼音码可依，交给引擎**按词推断**。
    //
    // ⚠️ 这条路径此前直接落到「逐字最常用读音」，于是五笔方案下词组注音系统性出错
    // （「行长」→ `xíng cháng`，两个字都错），而五笔用户恰恰是注音功能最主要的受众
    // ——打得出但不会读。推断走 `EngineManager::word_pinyin_syllables`，它枚举每字读音的
    // 笛卡尔积、取第一个**能在拼音词典里查回该词**的组合，查不回的组合直接排除。
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
/// 与 `wind-reverse` 的 `strip_tone` 平行但口径不同，刻意不合并：本函数产出给人看的
/// 注释文本（`ü` 保留、`ń`/`ḿ` 去调），那边产出与输入码比对的键（`ü`→`v`、小写、
/// 不处理 `ń`/`ḿ`）。改其一时核对另一处是否也该改。
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

/// 「模式 → 注释模板覆盖」映射。**唯一一处**把这层对应关系写死的地方——新增模式只加一行。
///
/// 返回 `None` = 该模式没有覆盖，跟随全局；`Some(s)` = 用 `s`（`s` 可能是空串，
/// 语义是「本模式不显示注释」——这正是三态里「缺失」与「空」必须分开的原因）。
///
/// 与 [`crate::layout::intent_for`] 同构（声明式重算，不做「进入时保存、退出时回放」），
/// 但**刻意不含 `add_word`**：加词面板走 `show_add_word_preview` 独立绘制路径，
/// 根本不经过 `comment_for`，给它加键只会是永远不生效的死配置。
/// 这两个函数的模式集合不同不是遗漏——布局要管所有会显示候选窗的路径，注释只管渲染注释的那条。
///
/// `overlay` = 当前特殊模式的 `[overlay]` 段快照（`State::overlay_spec`）。它与 `cfg`
/// 共用生命周期 `'a`，因为返回值可能借用其中任一方——快照存在 `State` 里正是为了让
/// 这个借用有处可依：特殊模式的配置已下沉到方案文件，每次现查注册表拿到的是临时值。
pub(crate) fn template_for<'a>(
    cfg: &'a Config,
    overlay: Option<&'a OverlaySpec>,
    active: Option<ModeKind>,
    vertical: bool,
) -> Option<&'a str> {
    let pick = |v: &'a CommentTemplateOverride,
                h: &'a CommentTemplateOverride|
     -> Option<&'a str> { if vertical { v.as_deref() } else { h.as_deref() } };
    match active {
        Some(ModeKind::Mix(i)) => cfg
            .schema
            .mix_modes
            .get(i as usize)
            .and_then(|m| pick(&m.comment_template_vertical, &m.comment_template_horizontal)),
        // 生僻字模式并入本支：它的 `overlay` 恒为 None（没有宿主方案、没有 [overlay] 段），
        // 于是 `and_then` 直接给出 None ＝ 跟随全局模板。这正是想要的默认档。
        // 反查模式同理（overlay 恒 None ⇒ 跟随全局），注释仍是引擎给的完整编码。
        Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
            overlay.and_then(|o| pick(&o.comment_template_vertical, &o.comment_template_horizontal))
        }
        Some(ModeKind::TempPinyin) => pick(
            &cfg.input.temp_pinyin.comment_template_vertical,
            &cfg.input.temp_pinyin.comment_template_horizontal,
        ),
        Some(ModeKind::TempEnglish) => pick(
            &cfg.input.temp_english.comment_template_vertical,
            &cfg.input.temp_english.comment_template_horizontal,
        ),
        Some(ModeKind::Url) => pick(
            &cfg.input.url.comment_template_vertical,
            &cfg.input.url.comment_template_horizontal,
        ),
        Some(ModeKind::Email) => pick(
            &cfg.input.email.comment_template_vertical,
            &cfg.input.email.comment_template_horizontal,
        ),
        Some(ModeKind::Unicode) => pick(
            &cfg.input.unicode.comment_template_vertical,
            &cfg.input.unicode.comment_template_horizontal,
        ),
        // 辅助码沿用主路径注释模板（辅助码只是筛选，注释来源仍是拼音主流程）。
        Some(ModeKind::AuxCode) => None,
        None => None,
    }
}

/// 方案级注释模板（`[candidate].comment_template_*`）。`None` = 本方案没意见。
pub(crate) fn schema_template_of(
    behavior: &wind_config::SchemaBehavior,
    vertical: bool,
) -> Option<&str> {
    if vertical {
        behavior.comment_template_vertical.as_deref()
    } else {
        behavior.comment_template_horizontal.as_deref()
    }
}

/// 三层裁决：**模式级 → 方案级 → 全局**。
///
/// # ★ 「没意见」的语义是「跟随**下一层**」，不是「跟随全局」
///
/// 加了方案层之后这两者不再等价。唯一能区分本实现与旧的两层实现的格子是
/// **「模式无意见 + 方案有意图 + 全局有值」**——旧实现给全局值，本实现给方案值。
/// 测试 `schema_layer_wins_when_mode_absent` 钉住它。
///
/// 三态里的第三态（空串 = 不显示）**在每一层都成立**：`Some("")` 是一个有意见的层，
/// 它压过下面所有层并渲染成空注释。故这里只能用 `Option::or`，不能顺手写成
/// 「非空才算数」——那会让「本模式/本方案不要注释」变得无法表达。
pub(crate) fn resolve_template<'a>(
    mode: Option<&'a str>,
    schema: Option<&'a str>,
    global: &'a str,
) -> &'a str {
    mode.or(schema).unwrap_or(global)
}

// ── 各求值入口认得的变量名 ──────────────────────────────────────────────────────
//
// 与各入口的 match 分支一一对应，测试 `entry_name_tables_match_their_bodies` 扫源码逐表核对
// （多一个、少一个都红）。设置页模板预览按场景取它们的并集判「此处可不可用」，与真实渲染的
// 求值链同一口径——另写一份清单的话，候选栏里原样回显的 `${word_code}` 在预览里会照常出值。

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
    "chaizi",
    "chaizi_code",
    "chaizi_all",
    "chaizi_code_all",
    "dict",
    "dict_all",
];

/// [`Coordinator::eval_text_var`] 自己的分支（其余交给 [`reverse_text_var`]）。
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
/// 从 [`Coordinator::eval_text_var`] 拆出来，是为了让悬停提示逐字段的对拍测试不构造协调器
/// 也能走到**生产同一份**求值代码：拆字列正是旧 `merge_chaizi_pinyin` 行序的判据，
/// 若测试里另写一份，对拍证明的就只是两份手写实现彼此一致。
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

/// 码表反查的全部码位（`word_codes_display` 以 `/` 连接）按 `sep` 重新连接，`/` 即分隔符。
fn joined_codes(codes: &str, sep: Option<&str>) -> VarValue {
    if codes.is_empty() {
        return VarValue::default();
    }
    VarValue::joined(codes.split('/'), sep.unwrap_or("/"))
}

impl crate::coordinator::Coordinator {
    /// 当前生效的注释模板：模式级 → 方案级 → 全局，见 [`resolve_template`]。
    ///
    /// 借用而非返回 String——它每次按键、每页候选前调一次，没必要为此分配。
    ///
    /// # ★ 方案层由调用方传入，而不是在这里取
    ///
    /// 方案级模板住在方案文件里，只能经 `EngineManager::active_behavior()` 拿到一个临时
    /// `Arc<SchemaBehavior>`——借用它的 `&str` 活不过本函数。调用方（`notify_ui_update`）
    /// 在候选循环外取一次存局部变量，模板借用它。
    ///
    /// ⛔ **不要改成把方案段快照进 `State`**（`[overlay]` 那份是那么做的）：快照要有失效点，
    /// 而 `schema_generation` **不随 `invalidate_schema` 递增**（设置页改 `schema_overrides`
    /// 不 bump 代际）⇒ 用户在设置页改完方案级模板，代际没变、快照不刷新，表现正是本仓
    /// 反复栽的「设置了不生效、重启后生效」。`behavior_cache` 则已在 `invalidate_schema`
    /// 里被清，每次现取才是对的。
    ///
    /// # ★ 方案层归属取 active，不取 `effective_data_schema`
    ///
    /// 注释模板是**呈现类**配置，与 `[candidate].layout` 同源取 active。临英/临拼/mix 的
    /// 注释需求**已经由模式层表达**，方案层再按 effective 解析一次就是两层说同一件事，
    /// 且两者可以互相矛盾。（注释**库**的过滤是数据类，另见 `ReverseLookup::comment_of`。）
    pub(crate) fn comment_template_for<'a>(
        &self,
        cfg: &'a Config,
        state: &'a State,
        behavior: &'a wind_config::SchemaBehavior,
        vertical: bool,
    ) -> &'a str {
        // 注释总开关在三层之上：关就是关（空模板 ⇒ 注释、上方注释条、上屏注释都为空）。
        if !cfg.ui.candidate.comment_enabled {
            return "";
        }
        resolve_template(
            template_for(cfg, state.overlay_spec.as_ref(), state.active, vertical),
            schema_template_of(behavior, vertical),
            cfg.ui.candidate.comment_template(vertical),
        )
    }

    /// overlay 反查模式（临时拼音 / 快捷输入）：码表用户借拼音反查编码的场景。
    ///
    /// 编码来源读 `input.temp_pinyin.code_hint_source`（见 [`Self::comment_hint_source`]），
    /// `${code_source}` 也据此视作「间接输入」。名字是旧「强制出码」时代的遗留——现在它
    /// 只判模式，不再无视配置强制放行。
    pub(crate) fn forces_code_hint(state: &State) -> bool {
        matches!(
            state.active,
            Some(ModeKind::TempPinyin) | Some(ModeKind::Mix(_))
        )
    }

    /// 注释段求值用的编码来源档。候选窗渲染与「上屏注释」（`input.alt_commit`）共用，
    /// 两处各算一份的话，同一条候选显示的注释与上屏的注释会不一样。
    ///
    /// 三份开关，模式层优先于方案层：
    /// - 临拼 / 快捷输入读 `input.temp_pinyin.code_hint_source`（出厂 `auto`，码表用户借
    ///   拼音反查编码的场景）——哪怕主方案是混输；
    /// - 混输方案读 `schema.mix.pinyin_code_hint`（出厂开，开 ⇒ 只放行码表反查：混输里的
    ///   拼音是全拼，双拼编码没意义）；
    /// - 其余（拼音 / 双拼主方案）读 `schema.pinyin.code_hint_source`（出厂 `off`）。
    ///
    /// 码表用户与纯拼音用户习惯相反，所以拆开。混输曾落在最后一档，拼音出厂改 `off` 后
    /// 混输跟着没了编码提示。
    /// ★ 临拼那份原样生效、**可以关掉**——旧实现在这里无视配置并集式强制放行反查。
    pub(crate) fn comment_hint_source(&self, state: &State) -> CodeHintSource {
        if Self::forces_code_hint(state) {
            return self.engine_mgr.temp_pinyin_code_hint_source();
        }
        let active = self.engine_mgr.active_schema_id();
        if self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed") {
            return if self.rt().config.schema.mix.pinyin_code_hint {
                CodeHintSource::CodeTable
            } else {
                CodeHintSource::Off
            };
        }
        self.engine_mgr.code_hint_source()
    }

    /// 「上屏注释 / 拼音」（`input.alt_commit`，t138）要上屏的文本；空串＝这条候选没有可上屏的。
    ///
    /// - `Pinyin` / `PinyinPlain`：与注释变量 `${pinyin}` 同一算法（[`pinyin_text`]），后者再去调。
    /// - `Comment`：当前生效的注释模板（与候选窗同一裁决：模式级 → 方案级 → 全局，排布方向
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
                let behavior = self.engine_mgr.active_behavior();
                let vertical = self.desired_orientation(state).vertical;
                let tpl = self.comment_template_for(&rt.config, state, &behavior, vertical);
                let fallback = self
                    .effective_data_schema(state)
                    .unwrap_or_else(|| self.engine_mgr.active_schema_id());
                let is_mix = matches!(state.active, Some(ModeKind::Mix(_)));
                let dict_schema = self.comment_dict_scope(state, c, is_mix, &fallback);
                render(tpl, 0, |name, arg| {
                    self.eval_var(
                        name,
                        arg,
                        c,
                        &reverse,
                        self.comment_hint_source(state),
                        &dict_schema,
                    )
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
        hint_source: CodeHintSource,
        dict_schema: &str,
    ) -> StyledText {
        tpl.render_whole(max_chars, &|name, arg| {
            self.eval_var(name, arg, c, reverse, hint_source, dict_schema)
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
        hint_source: CodeHintSource,
        dict_schema: &str,
    ) -> (StyledText, StyledText) {
        tpl.render_split(max_chars, &|name, arg| {
            self.eval_var(name, arg, c, reverse, hint_source, dict_schema)
        })
    }

    /// **任意文本**的反查渲染 —— cmdbar `dict.rev(text, n, format=…)` 的宿主侧实现。
    ///
    /// 与 [`Self::comment_for`] 共用模板渲染器与**变量词汇表**：用户在候选注释里学会的
    /// `${code}` / `${pinyin}` / `${chaizi}` 在这里同名同义，学一次。两处的变量求值
    /// （[`Self::eval_var`] / [`Self::eval_text_var`]）刻意挨着放 —— 它们是同一套词汇的
    /// 两个入口，分开写必然漂移。
    ///
    /// 与注释段的差别只在**取值对象**：那边是「当前候选」（带 source / code / boundary
    /// 等身份），这边是「一段裸文本」（剪贴板来的，没有候选身份）。故候选专属的
    /// `${code_hint}` 在这里不存在，写了会被渲染层原样回显让人看见。
    ///
    /// `max_chars` 传 0（不截断）：这里的产物**就是上屏文本**，不是显示投影。
    /// 候选窗的显示截断另有 `ui.candidate.max_chars` 在下游负责，两者不是一回事。
    ///
    /// ⚠️ **调用方不得已持有 `self.reverse` 的读锁**：本方法自取一次读锁，而 std 的
    /// `RwLock` 在有写者排队时同线程重入读会死锁（读锁不可重入）。当前唯一长期持有该
    /// 读锁的是 `notify_ui_update` 的候选渲染循环，它不经过短语求值，故无嵌套；
    /// 新增持锁路径时须回到这里核对。
    pub(crate) fn reverse_render(&self, text: &str, tpl: &str) -> String {
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
            let v = self.eval_text_var(name, arg, text, &reverse);
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
    /// - `code_rev` —— 恒取主码表反查，**不带**注释段那道 `hint_source && source==Pinyin`
    ///   门控。那道门控的理由是「码表方案下候选的码就是用户自己打的，反查是冗余」，而剪贴板
    ///   文本不是用户打出来的，反查正是这里的全部目的。
    /// - `shuangpin` —— 同义，但音节来路不同：那边有候选身份可用词条真值 `code`+`boundary`，
    ///   这边是裸文本，只能按词推断读音再编码。
    pub(crate) fn eval_text_var(
        &self,
        name: &str,
        arg: Option<&str>,
        text: &str,
        reverse: &wind_reverse::ReverseLookup,
    ) -> Option<VarValue> {
        Some(VarValue::from(match name {
            // 索引未就绪给空串（而不是 `None`）：`None` 在渲染层专表**未知变量名**，
            // 会原样输出 `${code_rev}` 让用户看见拼写错误。空串则不计入
            // `reverse_render` 的 found 判据，整条反查候选这一次照旧不出现 —— 想要的
            // 效果不变，但不会在「模板里还有别的非空变量」时把字面 `${code_rev}`
            // 混进上屏文本。索引建好后下一次按键即恢复。
            //
            // ★ 没就绪时顺手派后台构建（单飞、已就绪即返回，不阻塞）：运行期格式串不在配置里，
            // `DataNeeds` 看不见它，没有别人会替它建——不派的话这条反查永远是空的。
            "code_rev" | "code" => match self.engine_mgr.codetable_reverse_hint(text) {
                Some(code) => code,
                None => {
                    self.spawn_index_warm(&self.engine_mgr.primary_codetable_id(), false);
                    String::new()
                }
            },
            // `code_all` —— 该字在码表里的**全部**码位，默认 `/` 连接（`我` → `q/trn/trnt`）。
            //
            // 与 `code` 的分工照搬同文件 `chaizi` / `chaizi_all` 的既有惯例：不带后缀取单个，
            // `_all` 取全部、且可用 `${code_all:分隔符}` 换连接符。
            //
            // ★ 反查默认用它而不是 `code`：`codetable_reverse_hint` 取的是 `codes.last()`，
            // 而 `codes_of` 按**码长升序**，故它给的恒是最长的全码。那对候选注释是对的
            // （注释是候选右侧的窄条，塞下三个码会把候选行撑爆），但对「这个字怎么打」
            // 恰恰是最没用的答案 —— 简码才是用户要的。
            "code_rev_all" | "code_all" => {
                let sid = self.engine_mgr.code_source_schema();
                // 空串的理由同上面的 `code_rev`。
                let codes = match self.engine_mgr.word_codes_display(&sid, text) {
                    Some(codes) => codes,
                    None => {
                        self.spawn_index_warm(&sid, false); // 理由同上
                        String::new()
                    }
                };
                // `word_codes_display` 固定用 `/` 连接，换分隔符只能在这里替。
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
            // `shuangpin` —— 这段文本的双拼编码。
            //
            // 与注释段同义但取音节的路子不同：那边有候选身份，直接用词条真值
            // `code`+`boundary`；这边是裸文本，只能先按词推断读音（`word_pinyin_syllables`
            // 会做多音字消歧，「行长」得 `hang zhang` 而非 `xing chang`）。推不出读音
            // 就给空串 —— 与「查不到」同一档，这条反查候选不出现即可。
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

    /// 变量求值。`None` = 未知变量名（渲染层据此原样回显 `${name}` 让用户看见拼写错误）。
    ///
    /// 可用变量：
    /// - `code_hint` —— 引擎产的编码提示：码表前缀候选的**剩余编码**（输入 `si` 时 `sikao`
    ///   标 `kao`），以及混输的来源标记 `拼`。取自 `Candidate::comment`，这是该字段的
    ///   **唯一**消费点（其语义已收窄为「引擎产的编码提示」，不再是最终显示结果）。
    /// - `code_rev` —— 主码表**整词反查编码**（rev = reverse）。仅对拼音来源候选生效且受
    ///   `show_code_hint` 门控（临时拼音 / 快捷输入等反查模式强制开启）：码表方案下候选的
    ///   码就是用户自己打的，反查是冗余信息，故那里恒空。
    /// - `shuangpin` —— 这个候选的**双拼编码**。与 `code_rev` 正交：后者答「这词在主码表里
    ///   怎么打」（查词库反向索引），它答「这词的双拼怎么敲」（由 code+boundary 算）。
    ///   布局来源有回退链，全拼方案下装了双拼方案照样有值。
    ///
    /// ★ **`code` / `code_all` 是 `code_rev` / `code_rev_all` 的永久兼容别名**，不进文档站。
    ///   模板是用户在设置页手打的**自由文本**，不是 serde 键，没有 `RETIRED_KEYS` 那种静默
    ///   迁移路径；而 [`render_nodes`] 对未知变量名原样回显，删掉别名的后果是老用户的候选
    ///   旁边直接显示出字面 `${code}` 四个字符 —— 比「功能失效」更难看。留着的成本是这里
    ///   两条 match arm。
    /// - `pinyin` —— 带声调注音，见 [`pinyin_text`]。
    /// - `chaizi` —— 拆字字根串，**仅单字候选**。拆字回答的是「这个**字**由哪些字根构成」，
    ///   本就是单字概念；词组的字根串是各字字根的机械拼接，用户不会按字根记词，却足以把
    ///   候选行推得极宽（View 引擎**不支持文本折行**，超宽从窗口右缘硬裁，而注释恰在最右）。
    /// - `chaizi_code` —— 该字在拆字库里记录的**编码**，仅单字候选。与 `chaizi` 正交，
    ///   拼在一起即悬停提示拆字段的同款信息（`亻尔 [wq]`），但格式由模板决定而非写死。
    /// - `chaizi_all[:分隔符]` —— 不限字数的逐字字根，默认空格连接；带参数可改，
    ///   如 `${chaizi_all:／}` → `亻尔／女子`。长度自负（配 `comment_max_chars` 或只用于竖排）。
    /// - `chaizi_code_all[:分隔符]` —— 与 `chaizi_all` 对称，取的是逐字**编码**而非字根，
    ///   不限字数、默认空格连接、参数改分隔符规则相同（t207）。无编码的字同样跳过。
    /// - `dict` —— 用户挂载的注释词库（`[[ui.comment_dicts]]`）里该词的注释。键是**词**，
    ///   一份「英汉释义」「emoji 名称」可跨全部方案复用；候选 `code` 作可选消歧。
    /// - `dict_all[:分隔符]` —— 全部适用注释库**各出一条**，按挂载顺序、去重，默认 `；` 连接
    ///   （`dict` 只回第一个命中的库）。见 `ReverseLookup::comments_of`。
    ///
    /// `arg` = `${name:arg}` 的冒号后部分（未 trim）。只有声明支持参数的变量会读它，
    /// 其余变量收到参数时**静默忽略**而非报未知——参数写错不该让整个变量退化成错误回显。
    /// `${emoji}` 变量的取值：该词的 emoji（空格分隔，至多 `max_per_word` 个）。
    ///
    /// 只在 `show_as = "comment"` 档返回非空 —— 理由见调用点的注释（四档互斥，避免同一个
    /// emoji 既进候选又进注释）。功能关、表没加载、没命中一律 `None`；调用点把它换成空串，
    /// 模板据此整段消失。
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

    pub(crate) fn eval_var(
        &self,
        name: &str,
        arg: Option<&str>,
        c: &Candidate,
        reverse: &wind_reverse::ReverseLookup,
        hint_source: CodeHintSource,
        dict_schema: &str,
    ) -> Option<VarValue> {
        // 判据是 `chars().count()` 而非 `len()`：扩展区汉字走代理对，按字节数会被当成词组。
        let single = c.text.chars().count() == 1;
        Some(VarValue::from(match name {
            "code_hint" => c.comment.clone(),
            // `${emoji}` —— 该词对应的 emoji（`[input.emoji]`，空格分隔，取前 max_per_word 个）。
            //
            // ★ 门控是 `enabled && show_as == "comment"`，不是只看 `enabled`：`show_as` 是
            // 「emoji 以什么形态出现」的**单一决策点**，四档互斥。只看 enabled 的话，用户
            // 配了 `after` 又在模板里写了 `${emoji}`，同一个 emoji 会既进候选列表又出现在
            // 注释里，而两处都「按配置办事」，没人觉得自己错了。
            //
            // 本变量为空时按模板的可选段规则整段消失，故绝大多数候选不会留下空括号。
            // ★ 取不到给空串而不是 `?`：`None` 在渲染层是「未知变量名」，会原样回显
            // `${emoji}`——功能没开 / 不是 comment 档 / 没命中都属于「已知变量、这次为空」，
            // 应让所在可选段整段消失（同下面 `code_rev` 那条）。
            "emoji" => self.emoji_comment_of(&c.text).unwrap_or_default(),
            // ★ `unwrap_or_default()` 而不是 `?`：`None` 在渲染层的含义是**未知变量名**
            // （`render_nodes` 会原样输出 `${code_rev}` 并计作已填充，好让拼错的变量名
            // 显示出来）。反查索引没就绪属于「已知变量，这一次算不出」，给空串才对 ——
            // 空值会让 `${code_hint|code_rev}` 这样的回退链继续往下试，全空则整段消失。
            // 用 `?` 的话，切到大词库方案后的头几秒里，候选右边会挂着字面的
            // `${code_rev}` 四个字符。
            "code_rev" | "code" => {
                if hint_source.allows_reverse() && c.source == CandidateSource::Pinyin {
                    self.engine_mgr
                        .codetable_reverse_hint(&c.text)
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            // `code_rev_all` —— 全部码位（`我` → `q/trn/trnt`），`code_rev` 只给最长的那个全码。
            //
            // 门控与 `code_rev` **完全一致**（同为拼音来源候选才出）：理由也一样 ——
            // 码表方案下候选的码就是用户自己打的，反查是冗余信息。
            //
            // 与 [`Self::eval_text_var`] 的同名变量同义，两处必须一起改：用户在注释模板里
            // 学会的写法要能原样用在 `dict.rev(format=…)` 里，反之亦然。
            //
            // 出厂注释模板不含它 —— 三个码位会把候选行推得很宽，横排尤甚。它是给愿意
            // 用竖排、想一眼看全简码的用户的选项，不是默认。
            "code_rev_all" | "code_all" => {
                if hint_source.allows_reverse() && c.source == CandidateSource::Pinyin {
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
            // `shuangpin` —— 这个候选的**双拼编码**。
            //
            // 与 `code_rev` 正交，两者回答的是不同的问题：
            //   code_rev   这个词在主码表里怎么打 —— 查词库反向索引
            //   shuangpin  这个词的双拼怎么敲   —— 由 code+boundary 算出
            //
            // 双拼编码不存在于任何词库里（双拼词库就是全拼词库，双拼只是「全拼 + 一张
            // 键盘布局」），所以它只能算不能查。这也是 GH#128 放开码源方案限制解决不了
            // 的原因。
            //
            // ★ **全拼方案下照样有值**：布局来源有回退链（见 `shuangpin_hint_schema`），
            // 装了双拼方案就能用。会问「这个字双拼怎么敲」的，多半正是用全拼打字、
            // 想往双拼迁移的人。
            "shuangpin" => {
                if hint_source.allows_shuangpin() && c.source == CandidateSource::Pinyin {
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
            // 用户挂载的注释词库（`[[ui.comment_dicts]]`）。键是**词**，故一份库可跨方案复用；
            // 候选自身的 `code` 作可选消歧（注释库声明了 code 列时才生效，跨方案对不上则
            // 回落该词首条，见 `ReverseLookup::comment_of`）。
            // 注释库的 `schemas` 白名单在**查询时**求值，作用域取 `dict_schema`
            // （由调用方按 `effective_data_schema` 解析）：注释库是**数据类**资源，
            // 归属与词频/短语同源——临英下要按 `english` 查，而不是主方案。
            // 这与注释**模板**取 active（呈现类）刻意不同，见 `comment_template_for`。
            // `${dict:id}` 只查那一个库（`${dict}` 只回第一个命中的库，多库并排要靠它）。
            //
            // ★ 按输入大小写投影过的英文候选（`case_source` 有值，见 `apply_english_case`）用
            // **词库原文**查，且回退只往小写走（论坛 t281）：输入 `ASP` 时词库的 `asp`（毒蛇）
            // 显示成 `ASP`，拿显示形态查会精确命中 IT 库的缩写 `ASP`；拿原文 `asp` 双向回退
            // 同样会升格命中它。词库已确证这个词是小写的，升格就是换了个词。
            "dict" => {
                let (key, fb) = dict_query_key(c);
                reverse.comment_of_with(arg, key, Some(&c.code), dict_schema, fb)
            }
            // `${dict_all[:分隔符]}` —— 全部适用库各出一条（论坛 t282），默认 `；` 连接、按内容
            // 去重。`${dict}` 只回第一个命中的库，中英释义与 IT 术语库都收了同一个词时另一条
            // 就看不见；`${dict:id}` 能并排但要逐库写死 id。不带来源库标签：标签该写在库内容里。
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

/// 候选查注释库用的词与大小写回退方向（论坛 t281，理由见 `eval_var` 的 `dict` 分支）。
///
/// | 候选 | 查的词 | 回退 |
/// |---|---|---|
/// | 未改写 | 显示文本 | 双向（输入 `abc` 要能查到缩写 `ABC`） |
/// | 英文**词库**词被改写（投影 / 大小写档） | 词库原文 `case_source` | 只往小写 |
/// | 头部候选（用户所打原文及其变形）被大小写档改写 | **显示文本** | 只往小写 |
///
/// 头部候选不是词库词，`case_source` 只是用户打出来的形态，不说明它是哪个词；用户按出了
/// 哪种形态，就该按那种形态查——全大写档打 `asp` 得到的 `ASP` 要能查到缩写 `ASP`。但全小写档
/// 打 `ASP` 得到的 `asp` 是用户显式要的小写，双向回退会把它升格回 `ASP`，故同样只往小写走。
fn dict_query_key(c: &Candidate) -> (&str, wind_reverse::CaseFallback) {
    use wind_reverse::CaseFallback::{Both, LowerOnly};
    match &c.case_source {
        None => (c.text.as_str(), Both),
        Some(orig) if c.source == CandidateSource::English => (orig.as_str(), LowerOnly),
        Some(_) => (c.text.as_str(), LowerOnly),
    }
}

#[cfg(test)]
mod mode_template_tests {
    use super::*;
    use wind_config::config::MixModeConfig;

    /// 全局两份模板设成可辨认的值，模式级一律留空（跟随）。
    fn base_cfg() -> Config {
        let mut c = Config::default();
        c.ui.candidate.comment_template_vertical = "全局竖".into();
        c.ui.candidate.comment_template_horizontal = "全局横".into();
        c
    }

    /// 未配模式级覆盖时，各模式都取全局同方向那份——含「无模式」。
    #[test]
    fn absent_override_follows_global() {
        let c = base_cfg();
        for active in [
            None,
            Some(ModeKind::TempEnglish),
            Some(ModeKind::TempPinyin),
            Some(ModeKind::Url),
        ] {
            assert_eq!(template_for(&c, None, active, true), None, "{active:?} 竖");
            assert_eq!(template_for(&c, None, active, false), None, "{active:?} 横");
        }
    }

    /// ★★ 三态的第三态：空串 = 本模式不显示注释，**不等于**跟随全局。
    ///
    /// 这是 `Option<String>` 相对 `String` 的全部增量——用空串表达「跟随」的话，
    /// 「本模式不要注释」就没法表达了，而那正是本功能最主要的用途。
    #[test]
    fn empty_override_means_no_comment_not_follow() {
        let mut c = base_cfg();
        c.input.temp_pinyin.comment_template_vertical = Some(String::new());
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempPinyin), true),
            Some(""),
            "空串必须原样返回（= 本模式不显示），不能退化成 None（= 跟随全局）"
        );
        // 同模式的另一方向未配 → 仍跟随
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempPinyin), false),
            None
        );
    }

    /// ★ 横竖两个方向各自独立三态：只覆盖竖排时，横排仍跟随全局。
    #[test]
    fn directions_are_independent() {
        let mut c = base_cfg();
        c.input.temp_english.comment_template_vertical = Some("${dict}".into());
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempEnglish), true),
            Some("${dict}")
        );
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempEnglish), false),
            None,
            "只配了竖排，横排必须仍跟随全局"
        );
    }

    /// 覆盖只作用于本模式，别的模式与「无模式」不受影响。
    #[test]
    fn override_is_scoped_to_its_mode() {
        let mut c = base_cfg();
        c.input.temp_english.comment_template_vertical = Some("${dict}".into());
        assert_eq!(template_for(&c, None, None, true), None, "无模式不受影响");
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempPinyin), true),
            None,
            "别的模式不受影响"
        );
    }

    /// mix 实例按下标各自独立（其配置仍在 `Config` 里）。
    #[test]
    fn instance_modes_are_per_index() {
        let mut c = base_cfg();
        c.schema.mix_modes = vec![
            MixModeConfig {
                comment_template_vertical: Some("快捷用".into()),
                ..Default::default()
            },
            MixModeConfig::default(),
        ];
        assert_eq!(
            template_for(&c, None, Some(ModeKind::Mix(0)), true),
            Some("快捷用")
        );
        assert_eq!(template_for(&c, None, Some(ModeKind::Mix(1)), true), None);
    }

    /// 特殊模式的模板来自**方案文件的 `[overlay]` 段**（经 `State::overlay_spec` 快照传入），
    /// 不再来自 `Config`。下标只用于判「是不是 Special」，取值一律走 overlay 参数。
    #[test]
    fn special_mode_template_comes_from_overlay_spec() {
        let c = base_cfg();
        let ov = OverlaySpec {
            comment_template_horizontal: Some("快符用".into()),
            ..Default::default()
        };
        assert_eq!(
            template_for(&c, Some(&ov), Some(ModeKind::Special(0)), false),
            Some("快符用")
        );
        assert_eq!(
            template_for(&c, Some(&ov), Some(ModeKind::Special(0)), true),
            None,
            "只配了横排，竖排仍跟随全局"
        );
        // 下标不参与取值：换个下标、同一份快照，结果不变。
        assert_eq!(
            template_for(&c, Some(&ov), Some(ModeKind::Special(9)), false),
            Some("快符用")
        );
    }

    /// 没有快照（未进入特殊模式 / 该方案无 `[overlay]` 段）回落跟随全局，不 panic。
    /// mix 侧的下标越界（热重载删掉了该实例）同样回落。
    #[test]
    fn out_of_range_index_falls_back_to_follow() {
        let c = base_cfg();
        assert_eq!(template_for(&c, None, Some(ModeKind::Mix(7)), true), None);
        assert_eq!(
            template_for(&c, None, Some(ModeKind::Special(7)), true),
            None,
            "快照为 None 时不该 panic，跟随全局"
        );
    }

    /// ★ 方案层**不得**影响 `template_for` 本身——它只回答「模式怎么想」。
    ///
    /// 与 `layout::intent_for_answers_mode_layer_only` 同一条：分层的意义在于每层各答各的。
    /// 若图省事把方案意图折进 `template_for`，「模式没意见」与「模式没意见但方案有意见」
    /// 就再也分不开，将来想在两者之间插一层（如运行时手动值）便无处可插。
    #[test]
    fn template_for_answers_mode_layer_only() {
        let c = base_cfg();
        // 参数表里压根没有方案层，签名本身就是这条约束的载体；这里钉住「无模式 = None」，
        // 使「顺手在 None 分支里读方案」的改动当场变红。
        assert_eq!(template_for(&c, None, None, true), None);
        assert_eq!(template_for(&c, None, None, false), None);
    }
}

#[cfg(test)]
mod schema_layer_tests {
    //! 三层裁决（模式 → 方案 → 全局）的纯函数矩阵。端到端接线另见
    //! `coordinator::mode_comment_e2e_tests`。
    use super::*;

    fn behavior(v: Option<&str>, h: Option<&str>) -> wind_config::SchemaBehavior {
        wind_config::SchemaBehavior {
            comment_template_vertical: v.map(str::to_string),
            comment_template_horizontal: h.map(str::to_string),
            ..Default::default()
        }
    }

    /// ★★★ 唯一能区分三层与两层实现的格子。
    #[test]
    fn schema_layer_wins_when_mode_absent() {
        assert_eq!(resolve_template(None, Some("方案"), "全局"), "方案");
    }

    #[test]
    fn mode_layer_wins_over_schema() {
        assert_eq!(resolve_template(Some("模式"), Some("方案"), "全局"), "模式");
    }

    #[test]
    fn global_applies_only_when_no_layer_has_opinion() {
        assert_eq!(resolve_template(None, None, "全局"), "全局");
    }

    /// ★★ 空串在**每一层**都是「有意见」，压过下面所有层。
    ///
    /// 写成「非空才算数」的话，「本方案/本模式不要注释」就再也无法表达——
    /// 而那正是这套三态最主要的用途。
    #[test]
    fn empty_string_is_an_opinion_at_every_layer() {
        assert_eq!(resolve_template(None, Some(""), "全局"), "");
        assert_eq!(resolve_template(Some(""), Some("方案"), "全局"), "");
    }

    /// 横竖各自独立三态：只覆盖竖排、横排跟随，是合法且常见的配置。
    #[test]
    fn schema_directions_are_independent() {
        let b = behavior(Some("方案竖"), None);
        assert_eq!(schema_template_of(&b, true), Some("方案竖"));
        assert_eq!(schema_template_of(&b, false), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 求值闭包：按名取值；名字不在表里即「未知变量」。参数一律忽略。
    fn ev<'a>(
        pairs: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
        move |n, _arg| {
            pairs
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.to_string())
        }
    }

    /// 求值闭包：把收到的「名字 + 参数」原样回显，供参数语法的用例断言解析结果。
    fn echo_arg() -> impl Fn(&str, Option<&str>) -> Option<String> {
        |n, arg| Some(format!("{n}<{}>", arg.unwrap_or("∅")))
    }

    // ---------------- 出厂零回归 ----------------

    /// ★★ 出厂模板 `${code_hint|code}` 必须与本功能引入前的硬编码行为逐字节一致：
    /// 引擎产的剩余编码优先，为空则回退到主码表反查码，两者皆空则不显示。
    ///
    /// 这是整个改动的零回归闸门 —— 存量用户升级后不该看到任何变化。
    #[test]
    fn default_template_reproduces_legacy_behavior() {
        const T: &str = "${code_hint|code_rev|shuangpin}";
        // ① 引擎给了剩余码 → 用它（此时反查码即便存在也不参与，旧逻辑正是 if/else if）
        assert_eq!(
            render(
                T,
                0,
                ev(&[
                    ("code_hint", "kao"),
                    ("code_rev", "wq"),
                    ("shuangpin", "nihc")
                ])
            ),
            "kao"
        );
        // ② 引擎没给 → 回退反查码
        assert_eq!(
            render(T, 0, ev(&[("code_hint", ""), ("code_rev", "wq")])),
            "wq"
        );
        // ③ 都没有 → 不显示。
        // ⚠️ 三个变量**都要列出来**：`ev()` 对未列出的名字返回 `None`，而 `None` 的语义是
        // 「未知变量名」，渲染层会原样回显 ${shuangpin} 让人看见拼写错误。漏列一个，
        // 这条断言拿到的就是那串字面文本 —— 与真机行为无关，纯属夹具没喂全。
        assert_eq!(
            render(
                T,
                0,
                ev(&[("code_hint", ""), ("code_rev", ""), ("shuangpin", "")])
            ),
            ""
        );

        // ★ 第三段 `shuangpin` 是本轮新加的，必须证明它**不打扰老用户**：
        // 只要前两段任一非空，它就不参与。配了主码表的用户升级后所见分毫不变。
        assert_eq!(
            render(
                T,
                0,
                ev(&[("code_hint", ""), ("code_rev", "wq"), ("shuangpin", "nihc")])
            ),
            "wq",
            "有反查码时不该被击键码顶掉"
        );
        // ④ 前两段皆空时才轮到它 —— 那一格此前是空的（没配主码表的双拼用户），
        // 所以这不是回归，是把空白填上。
        assert_eq!(
            render(
                T,
                0,
                ev(&[("code_hint", ""), ("code_rev", ""), ("shuangpin", "nihc")])
            ),
            "nihc"
        );
    }

    // ---------------- 语法 ----------------

    #[test]
    fn plain_variable_and_literal_text() {
        assert_eq!(
            render("${pinyin}", 0, ev(&[("pinyin", "nǐ hǎo")])),
            "nǐ hǎo"
        );
        assert_eq!(
            render("拼:${pinyin}", 0, ev(&[("pinyin", "nǐ hǎo")])),
            "拼:nǐ hǎo"
        );
    }

    /// ★ 整个模板是隐式可选段：变量全空时**连字面文本一起**不显示，
    /// 否则会剩下一个孤零零的 `拼:`。
    #[test]
    fn all_vars_empty_hides_literal_text_too() {
        assert_eq!(render("拼:${pinyin}", 0, ev(&[("pinyin", "")])), "");
        assert_eq!(render("(${a} ${b})", 0, ev(&[("a", ""), ("b", "")])), "");
    }

    /// 无变量的纯文本模板同样不显示 —— 没有任何变量被填上，按隐式段规则整体消失。
    /// （想固定显示一段文字不是注释段的用途，那属于主题。）
    #[test]
    fn literal_only_template_shows_nothing() {
        assert_eq!(render("拼音", 0, ev(&[])), "");
    }

    #[test]
    fn fallback_takes_first_non_empty() {
        let e = ev(&[("a", ""), ("b", ""), ("c", "C")]);
        assert_eq!(render("${a|b|c}", 0, &e), "C");
        assert_eq!(render("${a|b}", 0, &e), "");
    }

    /// ★★ 可选段：段内变量全空则**整段消失**，含段内的装饰字符。
    ///
    /// 这是 `{}` 存在的全部理由 —— 没有它，`${code}{ (${pinyin})}` 在拼音为空时会显示
    /// `wq ()`，而空括号要配对解析才删得掉，靠 trim / 折叠空白救不了。
    #[test]
    fn optional_group_vanishes_when_all_its_vars_empty() {
        const T: &str = "${code}{ (${pinyin})}";
        assert_eq!(
            render(T, 0, ev(&[("code", "wq"), ("pinyin", "nǐ hǎo")])),
            "wq (nǐ hǎo)"
        );
        assert_eq!(
            render(T, 0, ev(&[("code", "wq"), ("pinyin", "")])),
            "wq",
            "拼音为空时整段消失，不得留下 `()`"
        );
    }

    /// 段内只要有**一个**变量非空，整段保留（空的那个按空串替换）。
    #[test]
    fn group_survives_if_any_var_filled() {
        const T: &str = "{(拼: ${pinyin} ${chaizi})}";
        assert_eq!(
            render(T, 0, ev(&[("pinyin", "nǐ hǎo"), ("chaizi", "女子")])),
            "(拼: nǐ hǎo 女子)"
        );
        assert_eq!(
            render(T, 0, ev(&[("pinyin", "nǐ hǎo"), ("chaizi", "")])),
            "(拼: nǐ hǎo)",
            "空变量须吞掉紧邻空白，否则是 `(拼: nǐ hǎo )`"
        );
        assert_eq!(render(T, 0, ev(&[("pinyin", ""), ("chaizi", "")])), "");
    }

    /// 空变量只吞**一个**紧邻空白——吞到底会把用户有意排的版式抹平。
    #[test]
    fn empty_var_eats_exactly_one_space() {
        assert_eq!(
            render("${a}   ${b}", 0, ev(&[("a", "A"), ("b", "")])),
            "A",
            "尾部空白由 trim 收拾"
        );
        assert_eq!(
            render("${a}   ${b}!", 0, ev(&[("a", "A"), ("b", "")])),
            "A  !",
            "只吞一个，其余留给用户的版式"
        );
    }

    /// ★ 段内变量的 `}` 不得被误当作段结束符——扫描段边界时必须跳过 `${…}`。
    /// 写错会让 `{(${pinyin})}` 解析成段 `(${pinyin`，后面 `)}` 变字面文本。
    #[test]
    fn group_scan_skips_variable_braces() {
        assert_eq!(
            render("{[${a}]}", 0, ev(&[("a", "X")])),
            "[X]",
            "段内变量的右花括号不是段结束"
        );
        assert_eq!(render("{[${a}]}", 0, ev(&[("a", "")])), "");
    }

    /// ★ 可选段可嵌套：内段只管自己的变量，外段在内外任一变量非空时保留。
    /// 悬停提示「拆字 / 拼音」合并段的模板 `{${chaizi}{ [${chaizi_code}]}\t}` 就是这个形态；
    /// 不配对的话内段会吞掉外段的右括号，`\t}` 沦为字面文本。
    #[test]
    fn groups_nest() {
        const T: &str = "{${a}{ [${b}]}|}${c}";
        let r = |a, b, c| render(T, 0, ev(&[("a", a), ("b", b), ("c", c)]));
        assert_eq!(r("A", "B", "C"), "A [B]|C");
        assert_eq!(r("A", "", "C"), "A|C", "内段消失不牵连外段");
        assert_eq!(r("", "", "C"), "C", "内外全空整段消失");
        assert_eq!(r("", "B", "C"), "[B]|C", "内段有值即撑住外段");
        // 未闭合的内段：外段配不上对，退化为字面文本，后面的段照常解析。
        assert_eq!(render("{x{${a}}", 0, ev(&[("a", "A")])), "{xA");
    }

    /// 计数谓词：不计数的变量照常输出，但撑不起整个模板或可选段。
    #[test]
    fn uncounted_var_renders_but_does_not_fill() {
        let e = ev(&[("char", "好"), ("r", "")]);
        let t = Template::parse("${char}：${r}");
        assert_eq!(t.render(&e, &|n| n != "char"), ("好：".to_string(), false));
        assert_eq!(t.render(&e, &|_| true), ("好：".to_string(), true));
        // 不 trim：首尾空白是内容（悬停提示的原始行要逐字节还原）。
        let lead = Template::parse("\t${char} ");
        assert_eq!(lead.render(&e, &|_| true).0, "\t好 ");
        assert!(lead.has_literal('\t'));
        assert!(
            !Template::parse("${char}").has_literal('\t'),
            "变量值不算字面"
        );
    }

    // ---------------- 变量参数 `${name:arg}` ----------------

    /// ★★ 参数**不 trim**：`${chaizi_all: · }` 里那两个空格正是用户要的分隔符。
    ///
    /// 名字仍 trim（`${ pinyin }` 这种手滑要认）。两者规则不同是有意的 ——
    /// 名字的空白一定是手滑，参数的空白一定是内容。
    #[test]
    fn variable_arg_preserves_whitespace_but_name_is_trimmed() {
        assert_eq!(
            render("${chaizi_all: · }", 0, echo_arg()),
            "chaizi_all< · >"
        );
        assert_eq!(render("${ pinyin }", 0, echo_arg()), "pinyin<∅>");
    }

    /// 只切**第一个**冒号——分隔符本身可以含冒号。
    #[test]
    fn variable_arg_splits_on_first_colon_only() {
        assert_eq!(render("${x:a:b}", 0, echo_arg()), "x<a:b>");
    }

    /// 参数可为空串（`${x:}`）：与「无参数」区分开，前者是「显式要求不加分隔」。
    #[test]
    fn empty_arg_differs_from_absent_arg() {
        assert_eq!(render("${x:}", 0, echo_arg()), "x<>");
        assert_eq!(render("${x}", 0, echo_arg()), "x<∅>");
    }

    /// 参数与回退链共存：每一段各自解析自己的参数。
    #[test]
    fn arg_works_inside_fallback_chain() {
        let e = |n: &str, arg: Option<&str>| match n {
            "a" => Some(String::new()), // 已知但空 → 回退
            "b" => Some(format!("B<{}>", arg.unwrap_or("∅"))),
            _ => None,
        };
        assert_eq!(render("${a:x|b:y}", 0, e), "B<y>");
    }

    /// ★ 未知变量名**原样回显**并计作已填充 —— 拼错一定看得见。
    ///
    /// 若把未知当空处理，用户得到的是「配了没反应」，得去翻文档才知道是拼写错误。
    #[test]
    fn unknown_variable_is_echoed_verbatim() {
        assert_eq!(
            render("${pinyn}", 0, ev(&[("pinyin", "nǐ hǎo")])),
            "${pinyn}"
        );
        // 回退链里遇到未知名即停（它是错误提示，不是"空值"，不该被跳过）。
        assert_eq!(
            render("${nope|pinyin}", 0, ev(&[("pinyin", "X")])),
            "${nope}"
        );
    }

    /// 语法写坏不 panic、不吞内容：未闭合的 `${` / `{` 退化成字面文本。
    /// 但字面文本里没有变量被填上 ⇒ 按隐式段规则整体不显示。
    #[test]
    fn malformed_template_degrades_to_literal_text() {
        assert_eq!(render("${unclosed", 0, ev(&[])), "");
        assert_eq!(render("{unclosed", 0, ev(&[])), "");
        // 与真变量混排时，字面部分原样保留。
        assert_eq!(render("${a} ${bad", 0, ev(&[("a", "A")])), "A ${bad");
    }

    /// 中文字面文本按字符推进，不得在 UTF-8 中间切开（切开会 panic）。
    #[test]
    fn multibyte_literal_text_is_safe() {
        assert_eq!(
            render("【读音】${pinyin}（完）", 0, ev(&[("pinyin", "nǐ")])),
            "【读音】nǐ（完）"
        );
    }

    // ---------------- 截断 ----------------

    #[test]
    fn max_chars_truncates_with_ellipsis() {
        let e = ev(&[("a", "zhōng guó rén")]);
        assert_eq!(render("${a}", 0, &e), "zhōng guó rén", "0 = 不限");
        assert_eq!(render("${a}", 5, &e), "zhōng…");
        // 按字符而非字节计——带声调字母是多字节，按字节会截在半个字符上。
        assert_eq!(render("${a}", 100, &e), "zhōng guó rén");
    }

    // ---------------- 变量求值：拼音 / 拆字 ----------------

    /// 边界真值切分：`你好` 的 `nihao` + `0b101`（音节起于字节 0 和 2）→ `["ni","hao"]`。
    #[test]
    fn boundary_splits_syllables() {
        assert_eq!(syllables_of("nihao", 0b101), vec!["ni", "hao"]);
        // 单音节的 boundary 是 0b1（「整串是一个音节」是真信息）。
        assert_eq!(syllables_of("hao", 0b1), vec!["hao"]);
        assert_eq!(
            syllables_of("zhongguoren", 1 | 1 << 5 | 1 << 8),
            vec!["zhong", "guo", "ren"]
        );
    }

    /// ★ `boundary == 0` 的语义是「**无边界信息**」而非「单音节」：不得把 `code` 整串当一个
    /// 音节，而要**降级到推断路径**（此前是降级到逐字首音，本轮改为推断）。
    ///
    /// 若把 0 当成「整串一个音节」，五笔码 `wqvb` 会被当作一个拼音音节送去筛读音表。
    #[test]
    fn zero_boundary_falls_through_to_inference() {
        let c = Candidate {
            text: "你好".into(),
            code: "nihao".into(),
            boundary: 0, // ← 要害
            source: CandidateSource::Pinyin,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |t| {
                    assert_eq!(t, "你好");
                    "ni hao".to_string()
                },
                |_, syls| {
                    assert_eq!(
                        syls,
                        Some(&["ni", "hao"][..]),
                        "boundary=0 应走推断，而非把 code 整串当一个音节"
                    );
                    "nǐ hǎo".to_string()
                }
            ),
            "nǐ hǎo"
        );
    }

    /// ★★ 非拼音来源（五笔候选）走**引擎按词推断**，而不是直接退到逐字首音。
    ///
    /// 这是本轮修的核心 bug：`boundary` 在码表方案下恒为 0、`code` 是形码，此前该路径直接
    /// 落到「逐字最常用读音」，于是「行长」显示成 `xíng cháng`（两个字都错）——而五笔用户
    /// 正是注音功能最主要的受众。断言落在**有没有把推断结果当音节传下去**上。
    ///
    /// 顺带钉住：即便 `boundary` 恰好有值也不得当拼音边界用（那是别的编码域的字段值）。
    #[test]
    fn non_pinyin_candidate_infers_syllables_instead_of_first_reading() {
        let c = Candidate {
            text: "行长".into(),
            code: "tfta".into(), // 五笔码
            boundary: 0b101,     // ← 有值，但不属于拼音域
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |t| {
                    assert_eq!(t, "行长", "推断应按候选文本而非编码");
                    "hang zhang".to_string()
                },
                |t, syls| {
                    assert_eq!(t, "行长");
                    assert_eq!(
                        syls,
                        Some(&["hang", "zhang"][..]),
                        "推断出的音节必须传下去消歧，否则「行长」会显示成 xíng cháng"
                    );
                    "háng zhǎng".to_string()
                }
            ),
            "háng zhǎng"
        );
    }

    /// 推断失败（含非汉字、生僻多音字超组合数护栏）→ 交由查表层逐字取最常用读音。
    /// 这是最后的兜底，不是常态路径。
    #[test]
    fn failed_inference_falls_back_to_per_char_readings() {
        let c = Candidate {
            text: "你好".into(),
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |_| String::new(), // 推断失败
                |_, syls| {
                    assert!(syls.is_none(), "推断失败时不得传入空音节序列");
                    "nǐ hǎo".to_string()
                }
            ),
            "nǐ hǎo"
        );
    }

    /// ★ 拼音来源候选**优先用词条真值，不走推断**。
    ///
    /// 词条自带的 `code`+`boundary` 比枚举笛卡尔积回查词典更可靠也更省。
    /// 闭包里 panic 是断言手段：一旦实现改成无条件先推断，会以「不该被调用」失败。
    #[test]
    fn pinyin_candidate_prefers_entry_truth_over_inference() {
        let c = Candidate {
            text: "行长".into(),
            code: "hangzhang".into(),
            boundary: 1 | 1 << 4, // hang|zhang
            source: CandidateSource::Pinyin,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |_| unreachable!("拼音来源候选不得走推断"),
                |_, syls| {
                    assert_eq!(
                        syls,
                        Some(&["hang", "zhang"][..]),
                        "词条音节须原样传下去消歧"
                    );
                    "háng zhǎng".to_string()
                }
            ),
            "háng zhǎng"
        );
    }
}

/// 上方注释条的拆分（`ui.candidate.comment_above`，设计 candidate-comment-above-line.md §3.2）。
///
/// ⚠️ 夹具的 `ev()` 对未列出的变量名返回 `None`（= 未知变量，原样回显），所以每条用例
/// 都要把模板里引用的变量列全；且模板必须含至少一个**非空**变量，否则整个模板按隐式
/// 可选段消失，拿到的 `("", "")` 与拆分逻辑无关。
#[cfg(test)]
mod split_tests {
    use super::*;

    fn ev<'a>(
        pairs: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
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
}

/// `eval_var` 的变量分发与门控。
///
/// ⚠️ 上面那三个测试模块全部走 **mock 求值闭包**，验的是模板语法；`eval_var` 本身
/// （变量名 → 数据源的分发、`hint_source && source==Pinyin` 那道门控）此前**一条测试
/// 都没有**。别名、`shuangpin` 这些都落在这里，所以补上。
///
/// 用仓库自带的 `data/` 作数据目录，不是 `build_dev/data`：这些用例只需要方案定义与
/// 双拼布局文件，不碰词典。`input_flow.rs` 那族 gate 在 build_dev 上，没数据时整族
/// 静默跳过（判据是耗时 0.00s），这里刻意不走那条路。
#[cfg(test)]
mod eval_var_tests {
    use super::*;
    use crate::coordinator::Coordinator;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
    }

    fn coord(active: &str) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.schema.available = vec![
            "shuangpin".to_string(),
            "pinyin".to_string(),
            "wubi86".to_string(),
        ];
        cfg.schema.active = active.to_string();
        Coordinator::new_headless(cfg, Some(&data_dir()))
    }

    /// 「你好」：小鹤下敲 `nihc`。
    fn nihao() -> Candidate {
        Candidate {
            text: "你好".into(),
            code: "nihao".into(),
            boundary: 0b101,
            source: CandidateSource::Pinyin,
            ..Default::default()
        }
    }

    fn eval(co: &Coordinator, name: &str, c: &Candidate, hint: CodeHintSource) -> Option<String> {
        let rev = wind_reverse::ReverseLookup::default();
        co.eval_var(name, None, c, &rev, hint, "shuangpin")
            .map(|v| v.to_string())
    }

    // ───────────── `${dict_all}`（论坛 t282） ─────────────

    /// 挂两个注释库：英汉释义 + IT 术语，都收了 `configure`。
    fn two_comment_libs(tag: &str) -> (PathBuf, wind_reverse::ReverseLookup) {
        let dir = std::env::temp_dir().join(format!("wind_dict_all_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let head = "name: t\ncolumns:\n  - text\n  - comment\n...\n";
        let mut specs = Vec::new();
        for (i, body) in ["configure\tv.配置\n", "configure\t[IT]配置…\n"]
            .iter()
            .enumerate()
        {
            let p = dir.join(format!("lib{i}.dict.yaml"));
            std::fs::write(&p, format!("{head}{body}")).unwrap();
            specs.push((p, Vec::new()));
        }
        let mut rl = wind_reverse::ReverseLookup::default();
        rl.reload_comments(&specs, None);
        (dir, rl)
    }

    fn word(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            code: text.into(),
            comment: "kh".into(),
            source: CandidateSource::English,
            ..Default::default()
        }
    }

    fn comment(
        co: &Coordinator,
        rl: &wind_reverse::ReverseLookup,
        tpl: &str,
        c: &Candidate,
    ) -> String {
        comment_max(co, rl, tpl, c, 0)
    }

    fn comment_max(
        co: &Coordinator,
        rl: &wind_reverse::ReverseLookup,
        tpl: &str,
        c: &Candidate,
        max: usize,
    ) -> String {
        co.comment_for(c, &Template::parse(tpl), max, rl, CodeHintSource::Off, "")
            .as_str()
            .to_string()
    }

    /// 默认分隔符 `；`，`${dict_all:/}` 自定义；`${dict}` 仍只回第一条。
    #[test]
    fn dict_all_joins_every_library_with_separator() {
        let co = coord("pinyin");
        let (dir, rl) = two_comment_libs("sep");
        let c = word("configure");
        assert_eq!(comment(&co, &rl, "${dict_all}", &c), "v.配置；[IT]配置…");
        assert_eq!(comment(&co, &rl, "${dict_all:/}", &c), "v.配置/[IT]配置…");
        assert_eq!(comment(&co, &rl, "${dict}", &c), "v.配置");
        // 分隔符参数不 trim：两侧空格原样保留。分隔符里不能有 `|`（变量回退链）与 `}`（变量
        // 结束），这里用模板写得出来的 ` · `。
        assert_eq!(
            comment(&co, &rl, "${dict_all: · }", &c),
            "v.配置 · [IT]配置…"
        );
        // 裸文本入口（cmdbar `dict.rev`）同义，同样经模板解析。
        assert_eq!(
            render("${dict_all: · }", 0, |n, a| co.eval_text_var(
                n,
                a,
                "configure",
                &rl
            )),
            "v.配置 · [IT]配置…"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `${dict_all}` 的连接符带分隔符标记（两个入口同义）。
    #[test]
    fn dict_all_marks_its_separator() {
        let co = coord("pinyin");
        let (dir, rl) = two_comment_libs("seprole");
        let c = word("configure");
        let t = co.comment_for(
            &c,
            &Template::parse("${dict_all}"),
            0,
            &rl,
            CodeHintSource::Off,
            "",
        );
        let sep: Vec<(u32, u32)> = t
            .spans()
            .iter()
            .filter(|s| s.separator)
            .map(|s| (s.start, s.end))
            .collect();
        assert_eq!(sep, vec![(8, 11)], "「v.配置」之后的「；」");
        assert!(t.spans().iter().all(|s| s.role == Some("dict")));
        let bare = co
            .eval_text_var("dict_all", Some("/"), "configure", &rl)
            .unwrap();
        assert_eq!(bare.seps, vec![(8, 9)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 查不到时是「已知变量、值为空」：所在可选段整段消失，而不是原样回显 `${dict_all}`。
    #[test]
    fn empty_dict_all_drops_its_optional_group() {
        let co = coord("pinyin");
        let (dir, rl) = two_comment_libs("empty");
        let tpl = "${code_hint}{ [${dict_all}]}";
        assert_eq!(comment(&co, &rl, tpl, &word("nosuch")), "kh");
        assert_eq!(
            comment(&co, &rl, tpl, &word("configure")),
            "kh [v.配置；[IT]配置…]"
        );
        assert_eq!(
            co.eval_text_var("dict_all", None, "nosuch", &rl).as_deref(),
            Some("")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 论坛 t281：输入 `ASP` 时词库的 `asp`（毒蛇）被投影成 `ASP`，不得贴上 IT 库里缩写
    /// `ASP` 的释义——`${dict}`、`${dict_all}` 都按词库原文 `asp` 查、回退不升格。
    #[test]
    fn case_projected_candidate_queries_dictionary_original() {
        use crate::english_candidates::{CaseVariant, apply_english_case};
        let co = coord("pinyin");
        let dir = std::env::temp_dir().join(format!("wind_t281_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lib = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(
                &p,
                format!("name: t\ncolumns:\n  - text\n  - comment\n...\n{body}"),
            )
            .unwrap();
            (p, Vec::new())
        };
        let it = lib("it.dict.yaml", "ASP\t[IT]动态服务器页面\n");
        let en = lib("en.dict.yaml", "asp\tn.毒蛇\n");

        let mut projected = [word("asp")];
        assert!(apply_english_case(
            &mut projected,
            "ASP",
            CaseVariant::Default
        ));
        let projected = &projected[0];
        assert_eq!(projected.text, "ASP", "前提：asp 被投影成 ASP");

        // 只有 IT 库：不显示缩写的释义。
        let mut rl = wind_reverse::ReverseLookup::default();
        rl.reload_comments(std::slice::from_ref(&it), None);
        assert_eq!(comment(&co, &rl, "${dict}", projected), "");
        assert_eq!(comment(&co, &rl, "{${dict_all}}", projected), "");
        // 词库里本来就是大写的 `ASP`（未投影）照常显示 IT 释义。
        assert_eq!(
            comment(&co, &rl, "${dict}", &word("ASP")),
            "[IT]动态服务器页面"
        );

        // 英汉库也收了小写 asp：显示毒蛇，聚合里同样没有 IT 那条。
        rl.reload_comments(&[en, it], None);
        assert_eq!(comment(&co, &rl, "${dict}", projected), "n.毒蛇");
        assert_eq!(comment(&co, &rl, "${dict_all}", projected), "n.毒蛇");
        assert_eq!(
            comment(&co, &rl, "${dict_all}", &word("ASP")),
            "[IT]动态服务器页面"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 头部候选（用户所打原文，非词库词）被大小写档改写时按**显示文本**查、只往小写回退
    /// （论坛 t281 审查）：全大写档打 `asp` → `ASP` 查得到缩写；全小写档打 `ASP` → `asp`
    /// 不得升格贴上缩写的释义。
    #[test]
    fn case_variant_head_candidate_queries_display_text() {
        use crate::english_candidates::{CaseVariant, apply_english_case, english_head_candidates};
        let co = coord("pinyin");
        let dir = std::env::temp_dir().join(format!("wind_t281h_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("it.dict.yaml");
        std::fs::write(
            &p,
            "name: t\ncolumns:\n  - text\n  - comment\n...\nASP\t[IT]动态服务器页面\n",
        )
        .unwrap();
        let mut rl = wind_reverse::ReverseLookup::default();
        rl.reload_comments(&[(p, Vec::new())], None);

        let mut up = english_head_candidates("asp", true, false);
        assert!(apply_english_case(&mut up, "asp", CaseVariant::Upper));
        assert_eq!(
            (up[0].text.as_str(), up[0].source),
            ("ASP", CandidateSource::None)
        );
        assert_eq!(comment(&co, &rl, "${dict}", &up[0]), "[IT]动态服务器页面");
        assert_eq!(
            comment(&co, &rl, "${dict_all}", &up[0]),
            "[IT]动态服务器页面"
        );

        let mut low = english_head_candidates("ASP", true, false);
        assert!(apply_english_case(&mut low, "ASP", CaseVariant::Lower));
        assert_eq!(low[0].text, "asp");
        assert_eq!(comment(&co, &rl, "${dict}", &low[0]), "");
        assert_eq!(comment(&co, &rl, "${dict_all}", &low[0]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 注释段的字数上限照样截断聚合结果。
    #[test]
    fn dict_all_obeys_comment_max_chars() {
        let co = coord("pinyin");
        let (dir, rl) = two_comment_libs("max");
        assert_eq!(
            comment_max(&co, &rl, "${dict_all}", &word("configure"), 6),
            "v.配置；[…"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 核心：双拼方案下 `${shuangpin}` 给出用户实际要敲的键。
    #[test]
    fn shuangpin_code_under_shuangpin_schema() {
        let co = coord("shuangpin");
        assert_eq!(
            eval(&co, "shuangpin", &nihao(), CodeHintSource::Auto).as_deref(),
            Some("nihc")
        );
    }

    /// ★★ **全拼方案下照样出双拼编码** —— 本轮放宽范围的核心。
    ///
    /// 早先只认活跃方案、全拼下恒空，理由是「全拼的击键就是拼音本身，显示是冗余」。
    /// 那句话对「本方案击键」成立，对「双拼编码」不成立：GH#128 的原话是「有时忘记了
    /// 还能看下」，而正在用双拼打字的人刚敲完码不会忘 —— 会忘并且想看一眼的，多半是
    /// **用全拼打字、正往双拼迁移**的人，他们恰恰是这个功能最主要的受众。
    ///
    /// 布局来源走回退链（`shuangpin_hint_schema`）：活跃方案不是双拼时，落到
    /// `primary_pinyin` 或 `available` 里首个双拼方案。此处 available 含 `shuangpin`。
    #[test]
    fn full_pinyin_schema_still_gets_shuangpin_code() {
        let co = coord("pinyin");
        assert_eq!(
            eval(&co, "shuangpin", &nihao(), CodeHintSource::Auto).as_deref(),
            Some("nihc"),
            "全拼方案应回退到已装的双拼方案取布局"
        );
    }

    /// 一个双拼方案都没装 ⇒ 恒空。回退链走到头也找不到布局，无从谈起。
    #[test]
    fn no_shuangpin_schema_installed_yields_empty() {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".to_string(), "wubi86".to_string()];
        cfg.schema.active = "pinyin".to_string();
        let co = Coordinator::new_headless(cfg, Some(&data_dir()));
        assert_eq!(
            eval(&co, "shuangpin", &nihao(), CodeHintSource::Auto).as_deref(),
            Some(""),
            "没装双拼方案时应是「已知变量但为空」，不是未知变量"
        );
    }

    /// ★ 别名等价：`code`/`code_all` 必须与 `code_rev`/`code_rev_all` 走同一个分支。
    ///
    /// 老用户模板里写的是 `${code}`，而 `render_nodes` 对未知变量名原样回显——别名断了
    /// 的表现不是「没反应」，是候选旁边直接显示出字面 `${code}` 四个字符。
    #[test]
    fn legacy_names_are_aliases() {
        let co = coord("shuangpin");
        let c = nihao();
        for (old, new) in [("code", "code_rev"), ("code_all", "code_rev_all")] {
            assert_eq!(
                eval(&co, old, &c, CodeHintSource::Auto),
                eval(&co, new, &c, CodeHintSource::Auto),
                "{old} 应与 {new} 等价"
            );
            assert!(
                eval(&co, old, &c, CodeHintSource::Auto).is_some(),
                "{old} 必须是已知变量名（None 会让候选旁显示字面 ${{{old}}}）"
            );
        }
    }

    /// 门控：开关关掉、或候选不是拼音来源时，三个编码变量一律为**空串**而非 None。
    ///
    /// 空串才会让 `${code_hint|code_rev}` 这样的回退链继续往下试；None 会中断回退并
    /// 回显变量名。
    #[test]
    fn gate_yields_empty_not_unknown() {
        let co = coord("shuangpin");
        let c = nihao();
        for name in ["code_rev", "code_rev_all", "shuangpin"] {
            assert_eq!(
                eval(&co, name, &c, CodeHintSource::Off).as_deref(),
                Some(""),
                "{name}：开关关掉应为空串"
            );
        }

        let codetable_cand = Candidate {
            source: CandidateSource::CodeTable,
            ..nihao()
        };
        for name in ["code_rev", "code_rev_all", "shuangpin"] {
            assert_eq!(
                eval(&co, name, &codetable_cand, CodeHintSource::Auto).as_deref(),
                Some(""),
                "{name}：非拼音来源候选应为空串"
            );
        }
    }

    /// ★ 四档开关各自放行哪个变量——`code_schema` 这一列。
    ///
    /// 要害是 **CodeTable 档下 `code_schema` 必须为空**：用户把来源设成「只看码表反查」，
    /// 哪怕模板里写着 `${code_schema}` 也不该冒出来。这正是「开关管允许哪些来源求值、
    /// 模板管按什么顺序和格式摆」那句分工的可验证形态。
    ///
    /// （`code_rev` 那一列在这里验不了：测试环境没有码表词库，它恒空，分不清是门控关掉
    /// 还是查不到。纯判据由 `wind-config` 侧的 `code_hint_source_gates` 钉。）
    #[test]
    fn hint_source_gates_shuangpin() {
        let co = coord("shuangpin");
        let c = nihao();
        for (src, expect) in [
            (CodeHintSource::Off, ""),
            (CodeHintSource::CodeTable, ""),
            (CodeHintSource::Shuangpin, "nihc"),
            (CodeHintSource::Auto, "nihc"),
        ] {
            assert_eq!(
                eval(&co, "shuangpin", &c, src).as_deref(),
                Some(expect),
                "{src:?} 档下 code_schema"
            );
        }
    }

    /// `boundary == 0` 的候选不出击键码——切分不可信，猜错了显示的就是错的编码。
    #[test]
    fn zero_boundary_candidate_has_no_keystroke_code() {
        let co = coord("shuangpin");
        let c = Candidate {
            boundary: 0,
            ..nihao()
        };
        assert_eq!(
            eval(&co, "shuangpin", &c, CodeHintSource::Auto).as_deref(),
            Some("")
        );
    }

    /// ★ 两个入口的**词汇表必须一致**：注释段认得的变量名，cmdbar 反查
    /// （`dict.rev`）也要认得，反之亦然。源码注释把这条写成了硬要求——
    /// 「用户在注释模板里学会的写法要能原样用在 `dict.rev(format=…)` 里」。
    ///
    /// 不一致的表现不是「少个功能」：`render_nodes` 对未知变量名原样回显，所以在
    /// 反查模板里写一个只有注释段认得的变量，**上屏文本里会混进字面 `${code_schema}`**。
    #[test]
    fn both_entries_share_the_same_vocabulary() {
        let co = coord("shuangpin");
        let rev = wind_reverse::ReverseLookup::default();
        // 编码族的全部名字（含兼容别名）在裸文本入口都必须是「已知变量」。
        for name in ["code_rev", "code", "code_rev_all", "code_all", "shuangpin"] {
            assert!(
                co.eval_text_var(name, None, "你好", &rev).is_some(),
                "{name} 在 dict.rev 入口应是已知变量名，否则上屏文本会混进字面 ${{{name}}}"
            );
        }
        // `code_hint` 是注释段**独有**的（裸文本没有候选身份，拿不到引擎产的提示），
        // 这条不对称是刻意的，一并钉住免得哪天被“顺手补齐”。
        assert_eq!(
            co.eval_text_var("code_hint", None, "你好", &rev),
            None,
            "code_hint 依赖候选身份，裸文本入口不该有"
        );
    }

    /// ★ `chaizi_code_all`（t207）—— 与 `chaizi_all` 对称，逐字反查编码，不限字数。
    ///
    /// 两处 match（候选注释 `eval_var` / 裸文本 `eval_text_var`）都要认得这个名字，
    /// 且都不受 `single`（仅单字）门控——这正是它与 `chaizi_code` 的分工差异。
    /// 测试环境的 `ReverseLookup::default()` 没有拆字数据，产出恒为空串，这里只钉
    /// 「已知变量名、且不受单字门控」，实际取值由 wind-reverse::codes_of 的单测钉住。
    #[test]
    fn chaizi_code_all_is_known_and_not_single_gated() {
        let co = coord("shuangpin");
        let rev = wind_reverse::ReverseLookup::default();
        // 候选注释入口：多字候选也应给出「已知变量」（空串而非 None）。
        let word = Candidate {
            text: "你好".into(),
            ..nihao()
        };
        assert_eq!(
            eval(&co, "chaizi_code_all", &word, CodeHintSource::Auto).as_deref(),
            Some(""),
            "无拆字数据时应为空串（已知变量），而非 None（未知变量名）"
        );
        // 裸文本入口（dict.rev）同一词汇表。
        assert!(
            co.eval_text_var("chaizi_code_all", None, "你好", &rev)
                .is_some(),
            "chaizi_code_all 在 dict.rev 入口也应是已知变量名"
        );
    }

    /// ⚠️ **本入口的 `code_schema` 只有「是不是已知变量名」被测到，产出的值没有。**
    ///
    /// 上面两条测试用仓库自带的 `data/` 建 Coordinator，而那里没有任何拼音词库
    /// （词库在 `build_dev/data`，本机构建产物）。于是 `word_pinyin_syllables("你好")`
    /// 推不出读音、返回空，`code_schema` 在裸文本入口恒为空串——断言只能比较
    /// `Some("") == Some("")`，抓得住「match arm 被删」，抓不住「算出来的值是错的」。
    ///
    /// 代码路径本身是对的（`generate_word_pinyin` 经 `SpacedCode` 产出空格分隔的**无声调**
    /// 音节，与 `ShuangpinReverse::encode_all` 的输入契约吻合），但那是读出来的结论，
    /// 不是测试钉住的。要真正覆盖，得照 `shuangpin_separator.rs` 的做法 gate 在
    /// `build_dev/data` 上另写一条。
    ///
    /// 裸文本入口的别名同样要等价。
    #[test]
    fn text_entry_legacy_names_are_aliases() {
        let co = coord("shuangpin");
        let rev = wind_reverse::ReverseLookup::default();
        for (old, new) in [("code", "code_rev"), ("code_all", "code_rev_all")] {
            assert_eq!(
                co.eval_text_var(old, None, "你好", &rev),
                co.eval_text_var(new, None, "你好", &rev),
                "{old} 应与 {new} 等价"
            );
        }
    }

    /// 未知变量名返回 `None`，好让用户看见自己拼错了。这条是上面几条的对照组：
    /// 没有它，「空串 vs None」的区分就只是我在注释里的主张。
    #[test]
    fn unknown_name_is_none() {
        let co = coord("shuangpin");
        assert_eq!(
            eval(&co, "code_rev_typo", &nihao(), CodeHintSource::Auto),
            None
        );
    }

    /// 拼音方案 / 临时拼音两份编码来源各配一值，建一个码表常驻的协调器。
    fn hint_coord(schema_src: &str, temp_src: &str) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".to_string(), "wubi86".to_string()];
        cfg.schema.active = "wubi86".to_string();
        cfg.schema.pinyin.code_hint_source = schema_src.to_string();
        cfg.input.temp_pinyin.code_hint_source = temp_src.to_string();
        Coordinator::new_headless(cfg, Some(&data_dir()))
    }

    fn hint_in(co: &Coordinator, active: Option<ModeKind>) -> CodeHintSource {
        let st = State {
            active,
            ..Default::default()
        };
        co.comment_hint_source(&st)
    }

    const ALL_SOURCES: [(&str, CodeHintSource); 4] = [
        ("off", CodeHintSource::Off),
        ("codetable", CodeHintSource::CodeTable),
        ("shuangpin", CodeHintSource::Shuangpin),
        ("auto", CodeHintSource::Auto),
    ];

    /// 临拼 / 快捷输入读 `input.temp_pinyin.code_hint_source`，原样取值、不做并集。
    ///
    /// 拼音方案那份刻意配成与之不同的值：两份若还串着读，这里会拿到对方的档位。
    #[test]
    fn overlay_modes_follow_temp_pinyin_source() {
        for (temp, want) in ALL_SOURCES {
            for schema in ["off", "auto"] {
                let co = hint_coord(schema, temp);
                for active in [Some(ModeKind::TempPinyin), Some(ModeKind::Mix(0))] {
                    assert_eq!(
                        hint_in(&co, active),
                        want,
                        "{active:?}：temp_pinyin={temp}, schema.pinyin={schema}"
                    );
                }
            }
        }
    }

    /// 主方案（含双拼）读 `schema.pinyin.code_hint_source`，不受临拼那份影响。
    #[test]
    fn main_schema_follows_schema_pinyin_source() {
        for (schema, want) in ALL_SOURCES {
            for temp in ["off", "auto"] {
                let co = hint_coord(schema, temp);
                assert_eq!(
                    hint_in(&co, None),
                    want,
                    "主方案：schema.pinyin={schema}, temp_pinyin={temp}"
                );
            }
        }
    }

    /// ★ 行为变化：临拼配成 `off` 时反查提示**真的关掉**。
    ///
    /// 旧实现对临拼 / 快捷输入无视配置、并集式强制放行反查（`forcing_reverse`），
    /// 用户关不掉；拆成独立开关后 off 必须生效，哪怕拼音方案那份开着。
    #[test]
    fn temp_pinyin_off_really_disables_reverse_hint() {
        let co = hint_coord("auto", "off");
        for active in [Some(ModeKind::TempPinyin), Some(ModeKind::Mix(0))] {
            let src = hint_in(&co, active);
            assert!(!src.allows_reverse(), "{active:?}：临拼 off 仍放行了反查");
            assert!(!src.allows_shuangpin(), "{active:?}：临拼 off 仍放行了双拼");
        }
    }

    /// 出厂：拼音方案不显示编码，临拼 / 快捷输入两种都放行。
    #[test]
    fn factory_defaults_split_by_mode() {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".to_string(), "wubi86".to_string()];
        cfg.schema.active = "wubi86".to_string();
        let co = Coordinator::new_headless(cfg, Some(&data_dir()));
        assert_eq!(hint_in(&co, None), CodeHintSource::Off);
        assert_eq!(
            hint_in(&co, Some(ModeKind::TempPinyin)),
            CodeHintSource::Auto
        );
        assert_eq!(hint_in(&co, Some(ModeKind::Mix(0))), CodeHintSource::Auto);
    }

    /// 混输方案为活跃方案、拼音那份与混输那份各配一值的协调器。
    fn mixed_coord(schema_src: &str, mix_hint: bool) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86_pinyin".to_string(), "pinyin".to_string()];
        cfg.schema.active = "wubi86_pinyin".to_string();
        cfg.schema.pinyin.code_hint_source = schema_src.to_string();
        cfg.schema.mix.pinyin_code_hint = mix_hint;
        Coordinator::new_headless(cfg, Some(&data_dir()))
    }

    /// 混输方案读 `schema.mix.pinyin_code_hint`，不受拼音方案那份影响。
    ///
    /// 拼音那份刻意配成与之相反的值：混输若还读拼音那份（本开关引入前的行为），这里会拿到
    /// 对方的档位。
    #[test]
    fn mixed_schema_follows_mix_switch() {
        for schema in ["off", "auto"] {
            assert_eq!(
                hint_in(&mixed_coord(schema, true), None),
                CodeHintSource::CodeTable,
                "混输开：schema.pinyin={schema}"
            );
            assert_eq!(
                hint_in(&mixed_coord(schema, false), None),
                CodeHintSource::Off,
                "混输关：schema.pinyin={schema}"
            );
        }
    }

    /// 混输方案下进临拼 / 快捷输入，仍按临拼那份——模式层优先于方案层。
    #[test]
    fn overlay_modes_in_mixed_schema_follow_temp_pinyin_source() {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86_pinyin".to_string(), "pinyin".to_string()];
        cfg.schema.active = "wubi86_pinyin".to_string();
        cfg.schema.mix.pinyin_code_hint = true;
        cfg.input.temp_pinyin.code_hint_source = "off".to_string();
        let co = Coordinator::new_headless(cfg, Some(&data_dir()));
        for active in [Some(ModeKind::TempPinyin), Some(ModeKind::Mix(0))] {
            assert_eq!(hint_in(&co, active), CodeHintSource::Off, "{active:?}");
        }
    }

    /// 出厂：拼音方案不显示编码，混输方案显示主码表反查编码。
    #[test]
    fn factory_mixed_schema_shows_reverse_code() {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86_pinyin".to_string(), "pinyin".to_string()];
        cfg.schema.active = "wubi86_pinyin".to_string();
        let co = Coordinator::new_headless(cfg, Some(&data_dir()));
        assert_eq!(hint_in(&co, None), CodeHintSource::CodeTable);
    }
}

/// 设置页预览行的模板样例求值与诊断（设计 text-span-colors.md §11）。
pub mod preview;

// 改动前的模板引擎逐字副本，只作对拍参照（见文件头）。
#[cfg(test)]
#[path = "comment_legacy_ref.rs"]
mod legacy_ref;

/// 纯文本输出与改动前逐字节相同（设计 text-span-colors.md §13.2 P2）：出厂与文档里的模板 ×
/// 各种取值夹具 × 截断上限 × 两种「算不算数」口径，逐一对拍改动前的引擎副本。
#[cfg(test)]
mod legacy_parity {
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
}

/// 内联色 `$[…]{}` 与片段角色（设计 text-span-colors.md §3.2、§4、§8.1）。
#[cfg(test)]
mod styled_tests {
    use super::*;
    use wind_theme::{Atom, ColorRef};

    fn ev<'a>(
        pairs: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
        move |n, _| {
            pairs
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.to_string())
        }
    }

    /// `(文字, 角色, 颜色名)` 逐段列出；颜色名取常态亮侧原子的名字 / `#` / `!`（非法）。
    fn spans(t: &StyledText) -> Vec<(String, Option<String>, Option<String>)> {
        t.spans()
            .iter()
            .map(|s| {
                let color = s.color.as_ref().map(|c| match &c.normal.light {
                    Atom::Name(n) => n.to_string(),
                    Atom::Rgba(_) => "#".to_string(),
                    Atom::Invalid => "!".to_string(),
                });
                (
                    t.as_str()[s.start as usize..s.end as usize].to_string(),
                    s.role.map(str::to_string),
                    color,
                )
            })
            .collect()
    }

    fn sp(
        text: &str,
        role: Option<&str>,
        color: Option<&str>,
    ) -> (String, Option<String>, Option<String>) {
        (text.into(), role.map(Into::into), color.map(Into::into))
    }

    /// 去掉模板里全部 `$[…]{` 与对应的 `}`（仅供「纯加法」断言，用例里的 BODY 不含别的 `}`
    /// 以外的结构时足够）：按解析树重建文字，不靠字符串替换。
    fn strip_colors(tpl: &str) -> String {
        fn walk(nodes: &[Node], out: &mut String) {
            for n in nodes {
                match n {
                    Node::Text(t) => out.push_str(t),
                    Node::Var(refs) => {
                        out.push_str("${");
                        let names: Vec<String> = refs
                            .iter()
                            .map(|r| match &r.arg {
                                Some(a) => format!("{}:{a}", r.name),
                                None => r.name.clone(),
                            })
                            .collect();
                        out.push_str(&names.join("|"));
                        out.push('}');
                    }
                    Node::Group(inner) => {
                        out.push('{');
                        walk(inner, out);
                        out.push('}');
                    }
                    Node::Color(_, inner) => walk(inner, out),
                }
            }
        }
        let mut out = String::new();
        walk(&parse(tpl), &mut out);
        out
    }

    /// 「加颜色」是纯加法：包上 `$[…]{}` 前后，文字输出逐字节相同（§4.3）。
    #[test]
    fn wrapping_in_color_is_purely_additive() {
        let cases = [
            "$[accent]{${a}}",
            "(${a} $[accent]{${b}})",
            "{$[accent]{(${a})}}",
            "$[accent]{{(${a})}}",
            "${a}$[#C00000/#FF8080]{(${b})}",
            "$[text_dim,selected=on_accent]{${a}}：$[warning]{${b}}",
            "${a}{ $[success]{[${b}]}}",
            "$[x]{$[y]{${a}} ${b}}",
            "$[红]{${a}}",
        ];
        let fixtures: [&[(&str, &str)]; 4] = [
            &[("a", "nǐ"), ("b", "亻尔")],
            &[("a", "nǐ"), ("b", "")],
            &[("a", ""), ("b", "亻尔")],
            &[("a", ""), ("b", "")],
        ];
        for tpl in cases {
            let plain = strip_colors(tpl);
            assert!(!plain.contains("$["), "{tpl}: 剥色后仍有 $[");
            for fx in fixtures {
                assert_eq!(
                    render(tpl, 0, ev(fx)),
                    render(&plain, 0, ev(fx)),
                    "{tpl} vs {plain}，取值 {fx:?}"
                );
                let t = Template::parse(tpl).render(&ev(fx), &|_| true);
                let p = Template::parse(&plain).render(&ev(fx), &|_| true);
                assert_eq!(t, p, "Template {tpl} vs {plain}，取值 {fx:?}");
            }
        }
    }

    /// 内联色只管上色、不是可选段：内部变量全空时它自己不消失（`()` 留着）。
    #[test]
    fn color_is_not_an_optional_group() {
        let t = render("${b}$[accent]{(${a})}", 0, ev(&[("a", ""), ("b", "x")]));
        assert_eq!(t, "x()");
        // 要「空则消失」就套一层可选段，两种写法等价。
        for tpl in ["${b}{$[accent]{(${a})}}", "${b}$[accent]{{(${a})}}"] {
            assert_eq!(render(tpl, 0, ev(&[("a", ""), ("b", "x")])), "x", "{tpl}");
        }
    }

    /// 内部变量计入外层「有值」判定：外层可选段因颜色段里的变量而保留。
    #[test]
    fn color_body_vars_count_for_enclosing_group() {
        let t = render("{[$[accent]{${a}}]}", 0, ev(&[("a", "x")]));
        assert_eq!(t, "[x]");
    }

    /// 空变量吞空白跨颜色边界照吞：`${chaizi}` 为空时 `)` 前不留空格；
    /// 同一模板经 `Template::render_styled`（不 trim）渲染，行尾也不多空格。
    #[test]
    fn whitespace_swallow_crosses_color_boundary() {
        let fx = [("pinyin", "nǐ"), ("chaizi", "")];
        assert_eq!(
            render("(${pinyin} $[accent]{${chaizi}})", 0, ev(&fx)),
            "(nǐ)"
        );
        let (t, _) = Template::parse("${pinyin} $[accent]{${chaizi}}").render_styled(
            &ev(&fx),
            &|_| true,
            false,
        );
        assert_eq!(t.as_str(), "nǐ");
        assert_eq!(spans(&t), vec![sp("nǐ", Some("pinyin"), None)]);
    }

    /// 角色：变量值 = 归一后的变量名（别名、回退链取到的那个）；字面 = literal；
    /// 未知变量回显无角色（不建区间）。
    #[test]
    fn roles_follow_the_variable_that_produced_the_text() {
        let fx = [("code_hint", ""), ("code", "wq"), ("pinyin", "nǐ")];
        let t = render_styled("${code_hint|code}(${pinyin}) ${pinyn}", 0, ev(&fx));
        assert_eq!(t.as_str(), "wq(nǐ) ${pinyn}");
        assert_eq!(
            spans(&t),
            vec![
                sp("wq", Some("code_rev"), None),
                sp("(", Some("literal"), None),
                sp("nǐ", Some("pinyin"), None),
                sp(") ", Some("literal"), None),
            ]
        );
        assert_eq!(role_of("code_all"), "code_rev_all");
    }

    /// 颜色栈：内联色标在其内的全部片段上（嵌套取内层），字面文字也带色。
    #[test]
    fn inline_color_marks_everything_inside() {
        let t = render_styled(
            "$[accent]{(${a}$[#F80]{${b}})}",
            0,
            ev(&[("a", "x"), ("b", "y")]),
        );
        assert_eq!(t.as_str(), "(xy)");
        assert_eq!(
            spans(&t),
            vec![
                sp("(", Some("literal"), Some("accent")),
                // `a` / `b` 不在契约清单里：没有角色，但仍带内联色。
                sp("x", None, Some("accent")),
                sp("y", None, Some("#")),
                sp(")", Some("literal"), Some("accent")),
            ]
        );
    }

    /// 语法成立与否只看结构，不看颜色写得对不对（§4.4）。
    #[test]
    fn syntax_validity_is_structural() {
        let fx = [("a", "x")];
        // 颜色非法：语法成立，只显示 BODY。
        for tpl in ["$[红]{${a}}", "$[]{${a}}", "$[#GG]{${a}}"] {
            let t = render_styled(tpl, 0, ev(&fx));
            assert_eq!(t.as_str(), "x", "{tpl}");
            let c = t.spans()[0].color.as_ref().expect("仍是内联色");
            assert_eq!(
                c.normal,
                ColorRef {
                    light: Atom::Invalid,
                    dark: Atom::Invalid
                }
            );
        }
        // 结构不成立：`$` 按字面、从 `[` 继续扫描——与引入内联色之前的引擎逐字节相同。
        let long = format!("$[{}]{{${{a}}}}", "a".repeat(65));
        for tpl in [
            "$[accent${a}",
            "$[accent]${a}",
            "$[acc\nent]{${a}}",
            "$[a{b]{${a}}",
            "$[a]{${a}",
            long.as_str(),
        ] {
            let got = render(tpl, 0, ev(&fx));
            assert!(got.starts_with("$["), "{tpl:?} 不成立，应留字面：{got}");
            assert_eq!(got, super::legacy_ref::render(tpl, 0, ev(&fx)), "{tpl:?}");
        }
        let ok64 = format!("$[{}]{{${{a}}}}", "a".repeat(64));
        assert_eq!(render(&ok64, 0, ev(&fx)), "x", "恰 64 字节成立");
    }

    /// 截断标记 `…` 继承被截处前一个字的样式。
    #[test]
    fn truncation_mark_inherits_style() {
        let t = render_styled("$[accent]{${a}}", 2, ev(&[("a", "abcd")]));
        assert_eq!(t.as_str(), "ab…");
        assert_eq!(spans(&t), vec![sp("ab…", None, Some("accent"))]);
    }

    /// 段名模板：字面文字角色 title、变量片段 in_title。
    #[test]
    fn title_context_marks_literals_and_vars() {
        let (t, _) = Template::parse("编码{(${code_source})}").render_styled(
            &ev(&[("code_source", "五笔")]),
            &|_| true,
            true,
        );
        assert_eq!(t.as_str(), "编码(五笔)");
        assert_eq!(
            spans(&t),
            vec![
                sp("编码(", Some("title"), None),
                sp("五笔", Some("code_source"), None),
                sp(")", Some("title"), None),
            ]
        );
        assert!(t.spans().iter().all(|s| s.in_title));
    }

    /// 可选段在内联色里（`$[x]{{…}}`）：段内片段照样带外层的内联色——`Group` 的子构建器
    /// 必须继承颜色栈，否则套一层可选段颜色就丢了。
    #[test]
    fn group_inside_color_keeps_the_color() {
        let t = render_styled("$[accent]{{(${code_rev})}}", 0, ev(&[("code_rev", "wq")]));
        assert_eq!(t.as_str(), "(wq)");
        assert_eq!(
            spans(&t),
            vec![
                sp("(", Some("literal"), Some("accent")),
                sp("wq", Some("code_rev"), Some("accent")),
                sp(")", Some("literal"), Some("accent")),
            ]
        );
    }

    /// 可选段 → 内联色 → 可选段 的嵌套：最内层取最近的内联色，段外的字面不带色。
    #[test]
    fn nested_group_color_group() {
        let t = render_styled("[{<$[warning]{{${pinyin}}!}>}]", 0, ev(&[("pinyin", "nǐ")]));
        assert_eq!(t.as_str(), "[<nǐ!>]");
        assert_eq!(
            spans(&t),
            vec![
                sp("[<", Some("literal"), None),
                sp("nǐ", Some("pinyin"), Some("warning")),
                sp("!", Some("literal"), Some("warning")),
                sp(">]", Some("literal"), None),
            ]
        );
    }

    /// 两个 walker 必须下钻 Color 节点（§8.1 ⚠️）。
    #[test]
    fn walkers_descend_into_color() {
        let t = Template::parse("$[x]{${debug}}");
        assert!(
            t.references("debug"),
            "漏下钻 ⇒ 调试上下文不准备、段静默为空"
        );
        let t = Template::parse("$[x]{a\tb}");
        assert!(t.has_literal('\t'), "漏下钻 ⇒ 分列段被折行");
    }
}

/// 入参防护：嵌套上限与线性配对（`MAX_DEPTH` / `Pairs`）。
/// 分隔符角色：`${name:分隔符}` 的分隔符带 `separator` 标记、角色仍是变量名；纯文本与改前逐字相同。
#[cfg(test)]
mod separator_tests {
    use super::*;
    use crate::coordinator::Coordinator;

    /// `(文字, 角色, 是分隔符)` 逐段。
    fn pieces(t: &StyledText) -> Vec<(&str, Option<&str>, bool)> {
        t.spans()
            .iter()
            .map(|s| {
                (
                    &t.as_str()[s.start as usize..s.end as usize],
                    s.role,
                    s.separator,
                )
            })
            .collect()
    }

    /// `name` 取 `items` 以参数（缺省 `default`）连接。
    fn items<'a>(
        name: &'a str,
        items: &'a [&'a str],
        default: &'a str,
    ) -> impl Fn(&str, Option<&str>) -> Option<VarValue> + 'a {
        move |n, arg| (n == name).then(|| VarValue::joined(items, arg.unwrap_or(default)))
    }

    #[test]
    fn separator_is_flagged_and_keeps_variable_role() {
        let t = render_styled(
            "${chaizi_all:／}",
            0,
            items("chaizi_all", &["亻尔", "女子"], " "),
        );
        assert_eq!(t.as_str(), "亻尔／女子");
        assert_eq!(
            pieces(&t),
            vec![
                ("亻尔", Some("chaizi_all"), false),
                ("／", Some("chaizi_all"), true),
                ("女子", Some("chaizi_all"), false),
            ]
        );
        // 别名归一后的角色同样带到分隔符上。
        let t = render_styled(
            "${dict_all}",
            0,
            items("dict_all", &["a", "b"], DICT_ALL_SEP),
        );
        assert_eq!(
            pieces(&t),
            vec![
                ("a", Some("dict"), false),
                ("；", Some("dict"), true),
                ("b", Some("dict"), false),
            ]
        );
    }

    /// 分隔符是空格时只标拼接处，值内部的空格不算——这正是不在渲染时按 `arg` 切开的理由。
    #[test]
    fn space_separator_does_not_split_inside_items() {
        let t = render_styled(
            "${dict_all: }",
            0,
            items("dict_all", &["a b", "c"], DICT_ALL_SEP),
        );
        assert_eq!(t.as_str(), "a b c");
        assert_eq!(
            pieces(&t),
            vec![
                ("a b", Some("dict"), false),
                (" ", Some("dict"), true),
                ("c", Some("dict"), false),
            ]
        );
    }

    /// 边界：空分隔符、单项不产分隔符片段；空分项照样占位（同 `join`）；全空 = 变量为空。
    #[test]
    fn edge_cases() {
        let t = render_styled("${chaizi_all:}", 0, items("chaizi_all", &["a", "b"], " "));
        assert_eq!(pieces(&t), vec![("ab", Some("chaizi_all"), false)]);
        let t = render_styled("${chaizi_all:/}", 0, items("chaizi_all", &["a"], " "));
        assert_eq!(pieces(&t), vec![("a", Some("chaizi_all"), false)]);
        let t = render_styled("${chaizi_all:/}", 0, items("chaizi_all", &["", "b"], " "));
        assert_eq!(
            pieces(&t),
            vec![
                ("/", Some("chaizi_all"), true),
                ("b", Some("chaizi_all"), false)
            ]
        );
        let tpl = "${code_hint}{(${chaizi_all:/})}";
        let e = |n: &str, a: Option<&str>| match n {
            "code_hint" => Some(VarValue::from("kh".to_string())),
            _ => items("chaizi_all", &[""], " ")(n, a),
        };
        assert_eq!(render(tpl, 0, e), "kh", "分项全空 = 变量为空，可选段消失");
        let t = render_styled("${chaizi_all:/}", 0, items("chaizi_all", &[], " "));
        assert!(t.is_empty());
    }

    /// 内联色里的分隔符：带内联色（内联色优先于一切角色色），仍有分隔符标记。
    #[test]
    fn separator_inside_inline_color() {
        let t = render_styled(
            "$[accent]{${chaizi_all:/}}",
            0,
            items("chaizi_all", &["a", "b"], " "),
        );
        let sp = &t.spans()[1];
        assert!(sp.separator && sp.color.is_some());
    }

    /// 纯文本：任意分项 × 分隔符，`VarValue` 与改前的 `join` 逐字相同；整条模板的纯文本出口
    /// （`render`）与返回改前字符串的求值闭包渲染结果相同。
    #[test]
    fn plain_text_matches_join() {
        let cases: &[&[&str]] = &[
            &[],
            &[""],
            &["a"],
            &["a", "b"],
            &["", "b"],
            &["a", ""],
            &["a b", "c d"],
        ];
        for parts in cases {
            for sep in ["", " ", "／", " · ", ":"] {
                let old = parts.join(sep);
                assert_eq!(&*VarValue::joined(*parts, sep), old, "{parts:?} {sep:?}");
                let tpl = format!("x{{(${{chaizi_all:{sep}}})}} ${{chaizi_all:{sep}}}");
                assert_eq!(
                    render(&tpl, 0, items("chaizi_all", parts, " ")),
                    render(&tpl, 0, |n, _| (n == "chaizi_all").then(|| old.clone())),
                    "{tpl:?} {parts:?}"
                );
            }
        }
    }

    /// `${code_rev_all}`：`word_codes_display` 的 `/` 连接串换分隔符，与改前的 `replace` 逐字相同。
    #[test]
    fn code_rev_all_matches_pre_change_replace() {
        let old = |codes: &str, arg: Option<&str>| match arg {
            Some(sep) if !codes.is_empty() => codes.replace('/', sep),
            _ => codes.to_string(),
        };
        for codes in ["", "q", "q/trn/trnt"] {
            for arg in [None, Some(""), Some(" "), Some("·")] {
                assert_eq!(
                    &*joined_codes(codes, arg),
                    old(codes, arg),
                    "{codes:?} {arg:?}"
                );
            }
        }
        // 缺省的 `/` 也是分隔符。
        assert_eq!(joined_codes("q/trn", None).seps, vec![(1, 2)]);
    }

    /// 分隔符的颜色（`None` = 无内联色），逐个分隔符片段。
    fn sep_colors(t: &StyledText) -> Vec<(&str, Option<InlineColor>)> {
        t.spans()
            .iter()
            .filter(|s| s.separator)
            .map(|s| {
                (
                    &t.as_str()[s.start as usize..s.end as usize],
                    s.color.as_deref().cloned(),
                )
            })
            .collect()
    }

    /// `${name:$[颜色]{分隔符}}`：变量收到的参数是 `分隔符`，颜色只标在分隔符上；文字不带标记。
    #[test]
    fn inline_color_in_separator_arg() {
        let e = items("chaizi_all", &["亻尔", "女子"], " ");
        let t = render_styled("${chaizi_all:$[#888888]{／}}", 0, &e);
        assert_eq!(t.as_str(), "亻尔／女子");
        assert_eq!(
            sep_colors(&t),
            vec![("／", Some(InlineColor::parse("#888888")))]
        );
        assert!(
            t.spans()
                .iter()
                .filter(|s| !s.separator)
                .all(|s| s.color.is_none()),
            "分项不带颜色"
        );
        assert_eq!(render("${chaizi_all:$[#888888]{／}}", 0, &e), "亻尔／女子");
        // 分隔符带空格、颜色写色名，同模板内联色。
        let t = render_styled("${chaizi_all:$[text_dim]{ · }}", 0, &e);
        assert_eq!(t.as_str(), "亻尔 · 女子");
        assert_eq!(
            sep_colors(&t),
            vec![(" · ", Some(InlineColor::parse("text_dim")))]
        );
        // 空分隔符照样成立：没有分隔符片段。
        let t = render_styled("${chaizi_all:$[x]{}}", 0, &e);
        assert_eq!(t.as_str(), "亻尔女子");
        assert!(sep_colors(&t).is_empty());
    }

    /// 优先级：分隔符自己的内联色 > 外层 `$[…]{}`（分项仍取外层）；没写时分隔符随外层。
    #[test]
    fn separator_inline_color_beats_outer_inline() {
        let e = items("chaizi_all", &["a", "b"], " ");
        let t = render_styled("$[accent]{${chaizi_all:$[#888]{/}}}", 0, &e);
        let got: Vec<(bool, InlineColor)> = t
            .spans()
            .iter()
            .map(|s| (s.separator, s.color.as_deref().cloned().unwrap()))
            .collect();
        assert_eq!(
            got,
            vec![
                (false, InlineColor::parse("accent")),
                (true, InlineColor::parse("#888")),
                (false, InlineColor::parse("accent")),
            ]
        );
    }

    /// 内联色里的 `|`、`:` 是分隔符文字，不切回退链；回退链照常工作。
    #[test]
    fn separator_body_is_not_split_by_fallback_bar() {
        let e = |n: &str, a: Option<&str>| match n {
            "chaizi_all" => Some(VarValue::default()),
            "dict_all" => Some(VarValue::joined(["x", "y"], a.unwrap_or(DICT_ALL_SEP))),
            _ => None,
        };
        let tpl = "${chaizi_all:$[a]{|}|dict_all:$[b]{:|}}";
        let t = render_styled(tpl, 0, e);
        assert_eq!(t.as_str(), "x:|y");
        assert_eq!(sep_colors(&t), vec![(":|", Some(InlineColor::parse("b")))]);
        let e2 = items("chaizi_all", &["a", "b"], " ");
        assert_eq!(render("${chaizi_all:$[a]{|}|dict_all}", 0, &e2), "a|b");
    }

    /// 出错口径同模板内联色：语法不成立（`]` 后不是 `{`、`SPEC` 过长或含换行、只占参数一部分）
    /// 参数按字面；`BODY` 未闭合即变量未闭合，其后全按字面；颜色写错仍是内联色（求色时回落正文色）。
    #[test]
    fn separator_inline_color_errors_degrade_like_inline_color() {
        let e = items("chaizi_all", &["a", "b"], " ");
        let plain = |tpl: &str| render(tpl, 0, &e);
        assert_eq!(plain("${chaizi_all:$[x]/}"), "a$[x]/b", "]后不是 {{");
        assert_eq!(
            plain("${chaizi_all:-$[x]{/}}"),
            "a-$[x]{/}b",
            "只占一部分：整个按字面"
        );
        assert!(
            sep_colors(&render_styled("${chaizi_all:-$[x]{/}}", 0, &e))[0]
                .1
                .is_none()
        );
        let long = "x".repeat(COLOR_SPEC_MAX + 1);
        assert_eq!(
            plain(&format!("${{chaizi_all:$[{long}]{{/}}}}")),
            format!("a$[{long}]{{/b}}"),
            "SPEC 过长：同改前，变量收在第一个 `}}`"
        );
        assert_eq!(
            plain("${chaizi_all} ${chaizi_all:$[x]{/ {${chaizi_all}"),
            "a b ${chaizi_all:$[x]{/ {${chaizi_all}",
            "BODY 未闭合 = 变量未闭合，其后全按字面"
        );
        // 颜色值非法：语法成立，分隔符带一个非法颜色（求色回落正文色，同 `$[红]{…}`）。
        let t = render_styled("${chaizi_all:$[红]{/}}", 0, &e);
        assert_eq!(t.as_str(), "a/b");
        assert_eq!(sep_colors(&t), vec![("/", Some(InlineColor::parse("红")))]);
        assert_eq!(
            InlineColor::parse("红").normal,
            wind_theme::ColorRef {
                light: wind_theme::Atom::Invalid,
                dark: wind_theme::Atom::Invalid,
            }
        );
    }

    /// 夹具拆字表：好 = 女子 [vbg]、人 = 人 [w]、丂 = （空） [gnv]（有编码无字根）。
    fn chaizi() -> wind_reverse::ReverseLookup {
        let dir = std::env::temp_dir().join(format!("wind_sep_chaizi_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("chaizi.txt");
        std::fs::write(&p, "好\t女子\tvbg\n人\t人\tw\n丂\t\tgnv\n").unwrap();
        let rl = wind_reverse::ReverseLookup::load(None, Some(&p));
        let _ = std::fs::remove_dir_all(&dir);
        rl
    }

    /// `${chaizi_all}` / `${chaizi_code_all}` 两个入口（裸文本、候选）的纯文本与改前的
    /// `radicals_of` / `codes_of` 逐字相同，分隔符带标记。
    #[test]
    fn chaizi_all_variables_match_pre_change() {
        let rl = chaizi();
        let co = Coordinator::new_headless(Config::default(), None);
        for text in ["好人", "好", "丂人", "好X人", "XY"] {
            let cand = Candidate {
                text: text.into(),
                ..Default::default()
            };
            for arg in [None, Some(""), Some("／"), Some(" · ")] {
                let sep = arg.unwrap_or(" ");
                let old = [
                    ("chaizi_all", rl.radicals_of(text, sep)),
                    ("chaizi_code_all", rl.codes_of(text, sep)),
                ];
                for (name, old) in old {
                    let bare = reverse_text_var(name, arg, text, &rl).unwrap();
                    assert_eq!(&*bare, old, "{name} {text:?} {arg:?}");
                    let via = co
                        .eval_var(name, arg, &cand, &rl, CodeHintSource::Off, "")
                        .unwrap();
                    assert_eq!(via, bare, "{name} {text:?} {arg:?}");
                }
            }
        }
        let tpl = Template::parse("${chaizi_all:／} ${chaizi_code_all:+}");
        let cand = Candidate {
            text: "好人".into(),
            ..Default::default()
        };
        let t = co.comment_for(&cand, &tpl, 0, &rl, CodeHintSource::Off, "");
        assert_eq!(t.as_str(), "女子／人 vbg+w");
        let seps: Vec<(&str, Option<&str>)> = pieces(&t)
            .into_iter()
            .filter(|p| p.2)
            .map(|p| (p.0, p.1))
            .collect();
        assert_eq!(
            seps,
            vec![("／", Some("chaizi_all")), ("+", Some("chaizi_code_all"))]
        );
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;

    /// 逐次重扫的变量收尾：`from` 起第一个不在参数内联色 `$[SPEC]{BODY}` 里的 `}`。
    /// 朴素写法（逐处现找），与配对表的查表实现对拍。
    fn rescan_var_end(b: &[u8], from: usize) -> Option<usize> {
        let mut j = from;
        while j < b.len() {
            if b[j] == b'$' && b.get(j + 1) == Some(&b'[') {
                let spec = (j + 2..b.len().min(j + 2 + COLOR_SPEC_MAX + 1))
                    .find(|&k| matches!(b[k], b']' | b'{' | b'}' | b'\n'));
                if let Some(k) = spec
                    && b[k] == b']'
                    && b.get(k + 1) == Some(&b'{')
                    && let Some(body) = (k + 2..b.len()).find(|&m| b[m] == b'}')
                {
                    j = body + 1;
                    continue;
                }
            }
            if b[j] == b'}' {
                return Some(j);
            }
            j += 1;
        }
        None
    }

    /// 逐次重扫实现，留作对拍基准（配对规则必须逐字相同）。
    fn rescan_group_end(b: &[u8], from: usize) -> Option<usize> {
        let mut i = from;
        let mut depth = 0usize;
        while i < b.len() {
            if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
                i = rescan_var_end(b, i + 2)? + 1;
                continue;
            }
            match b[i] {
                b'{' => depth += 1,
                b'}' if depth == 0 => return Some(i),
                b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// 配对表与旧的逐次重扫在随机模板上逐位置一致（凡旧解析会去找配对的 `{`）。
    #[test]
    fn pairs_match_rescan_reference() {
        let alphabet = b"${}[]a|";
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let len = (next() % 24) as usize;
            let b: Vec<u8> = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            let pairs = Pairs::new(&b);
            // 只比「解析器会问到的」`{`：扫到它时它是记号边界（不在某个 `${…}` 里）。
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
                    let want = rescan_var_end(&b, i + 2);
                    match want {
                        Some(c) => {
                            assert_eq!(
                                pairs.var_end(i + 2, b.len()),
                                Some(c),
                                "{:?} @ {i}",
                                String::from_utf8_lossy(&b)
                            );
                            i = c + 1;
                            continue;
                        }
                        // 未闭合：之后一切按字面，配对表不再回答（全为 None）。
                        None => break,
                    }
                }
                if b[i] == b'{' {
                    assert_eq!(
                        pairs.group_end(i, b.len()),
                        rescan_group_end(&b, i + 1),
                        "{:?} @ {i}",
                        String::from_utf8_lossy(&b)
                    );
                }
                i += 1;
            }
        }
    }

    fn eval(name: &str, _: Option<&str>) -> Option<String> {
        (name == "a").then(|| "A".to_string())
    }

    /// 深度 5000 的嵌套不打穿栈：超过上限的层按字面文字输出，上限以内照常求值。
    #[test]
    fn deep_nesting_is_bounded() {
        let n = 5000;
        for (open, close) in [("{", "}"), ("$[accent]{", "}")] {
            let tpl = format!("{}${{a}}{}", open.repeat(n), close.repeat(n));
            let out = render(&tpl, 0, eval);
            assert!(out.contains('A'), "变量照常求值");
            // 上限以内的层被解析吃掉，超出的层原样留在输出里。
            let literal_opens = out.matches('{').count();
            assert_eq!(literal_opens, n - MAX_DEPTH, "{open}");
        }
    }

    /// 20 万个未闭合的 `{`（以及 `${`）在毫秒级完成：配对是一次线性扫描。
    #[test]
    fn unclosed_braces_are_linear() {
        for tpl in [
            "{".repeat(200_000),
            "${".repeat(100_000),
            "{${a}".repeat(40_000),
        ] {
            let t = std::time::Instant::now();
            let _ = Template::parse(&tpl);
            // 平方级实测 37 秒；线性扫描在调试构建下也远低于这个宽松上限。
            assert!(
                t.elapsed() < std::time::Duration::from_secs(2),
                "{:?}",
                t.elapsed()
            );
        }
    }
}

/// `TEXT_ROLES` 契约清单 ⊇ 全部求值入口认得的变量名（设计 text-span-colors.md §3.1）。
///
/// 反方向的守卫：`Span::role` 取自清单，清单外的变量不产角色、主题给它配的色静默不生效。
/// 求值入口是若干 `match` / 闭包，没有可枚举的集合，故直接扫源码——抽出各入口函数体里的
/// `"名字" =>` / `"名字" |` / `"名字" if` 分支，经别名表归一后逐个核对。新增变量忘了进清单，
/// 这里就红。
#[cfg(test)]
mod role_contract_tests {
    use super::role_of;

    /// 从 `from` 起第一个 `{` 开始、按花括号配对截出函数 / 闭包体（跳过字符串字面量）。
    fn body_after<'a>(src: &'a str, anchor: &str) -> &'a str {
        let at = src
            .find(anchor)
            .unwrap_or_else(|| panic!("源码里找不到 {anchor:?}——入口改名了？同步改这条测试"));
        let b = src.as_bytes();
        let open = at + src[at..].find('{').expect("入口后应有函数体");
        let (mut depth, mut i, mut in_str) = (0usize, open, false);
        while i < b.len() {
            match (in_str, b[i]) {
                (true, b'\\') => i += 1,
                (true, b'"') => in_str = false,
                (false, b'"') => in_str = true,
                (false, b'{') => depth += 1,
                (false, b'}') => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[open..=i];
                    }
                }
                _ => {}
            }
            i += 1;
        }
        panic!("{anchor:?} 的函数体没闭合");
    }

    /// 抽出 `"名字"` 后紧跟 `=>`、`|`、`if` 的变量名（match 分支的形态）。
    fn arm_names(body: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = body;
        while let Some(q) = rest.find('"') {
            let after = &rest[q + 1..];
            let Some(end) = after.find('"') else { break };
            let name = &after[..end];
            let tail = after[end + 1..].trim_start();
            let is_arm = tail.starts_with("=>")
                || (tail.starts_with('|') && !tail.starts_with("||"))
                || tail.starts_with("if ");
            if is_arm
                && !name.is_empty()
                && name.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
            {
                out.push(name.to_string());
            }
            rest = &after[end + 1..];
        }
        out
    }

    fn entry_names(extra: Option<(&str, &str)>) -> Vec<String> {
        let comment = include_str!("comment.rs");
        let tooltip = include_str!("tooltip.rs");
        let coordinator = include_str!("coordinator.rs");
        let mut bodies = vec![
            body_after(comment, "pub(crate) fn eval_var("),
            body_after(comment, "pub(crate) fn eval_text_var("),
            body_after(comment, "pub(crate) fn reverse_text_var("),
            body_after(comment, "pub(crate) fn role_of("),
            body_after(tooltip, "pub(crate) fn char_var("),
            body_after(tooltip, "let cand_eval = |name: &str, arg: Option<&str>|"),
            body_after(
                coordinator,
                "let cand_eval = |name: &str, arg: Option<&str>|",
            ),
        ];
        if let Some((src, anchor)) = extra {
            bodies.push(body_after(src, anchor));
        }
        bodies.into_iter().flat_map(arm_names).collect()
    }

    fn unlisted(names: &[String]) -> Vec<String> {
        let mut out: Vec<String> = names
            .iter()
            .map(|n| role_of(n).to_string())
            .filter(|r| !wind_ui_types::TEXT_ROLES.contains(&r.as_str()))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// 各入口旁的变量名常量表与入口的 match 分支逐一相符（设置页预览按它们判场景可用性）。
    #[test]
    fn entry_name_tables_match_their_bodies() {
        let comment = include_str!("comment.rs");
        let tooltip = include_str!("tooltip.rs");
        let coordinator = include_str!("coordinator.rs");
        let cases: [(&str, &str, &[&str]); 6] = [
            (comment, "pub(crate) fn eval_var(", super::EVAL_VAR_NAMES),
            (
                comment,
                "pub(crate) fn eval_text_var(",
                super::EVAL_TEXT_VAR_NAMES,
            ),
            (
                comment,
                "pub(crate) fn reverse_text_var(",
                super::REVERSE_TEXT_VAR_NAMES,
            ),
            (
                tooltip,
                "pub(crate) fn char_var(",
                crate::tooltip::CHAR_VAR_NAMES,
            ),
            (
                tooltip,
                "let cand_eval = |name: &str, arg: Option<&str>|",
                crate::tooltip::TOOLTIP_CAND_VAR_NAMES,
            ),
            (
                coordinator,
                "let cand_eval = |name: &str, arg: Option<&str>|",
                crate::tooltip::TOOLTIP_COORD_VAR_NAMES,
            ),
        ];
        for (src, anchor, table) in cases {
            let mut got = arm_names(body_after(src, anchor));
            got.sort();
            got.dedup();
            let mut want: Vec<String> = table.iter().map(|s| s.to_string()).collect();
            want.sort();
            assert_eq!(got, want, "{anchor} 的分支与常量表对不上");
        }
    }

    #[test]
    fn every_evaluable_variable_is_a_listed_role() {
        let names = entry_names(None);
        // 防空转：几个确定存在的变量必须被扫到。
        for must in [
            "code_hint",
            "readings",
            "word_code",
            "full_text",
            "char",
            "code",
        ] {
            assert!(
                names.iter().any(|n| n == must),
                "扫描漏了 {must}：{names:?}"
            );
        }
        assert_eq!(
            unlisted(&names),
            Vec::<String>::new(),
            "这些变量能求值却不在 TEXT_ROLES 里——主题给它们配的角色色会静默不生效"
        );
    }

    /// 扫描确实抓得住：往一个 match 里塞个清单外的名字就报出来。
    #[test]
    fn scan_catches_an_unlisted_arm() {
        const FAKE: &str = r#"fn fake(name: &str) -> Option<String> {
            Some(match name { "pinyin" => x(), "no_such_role" if y => z(), _ => return None })
        }"#;
        let names = entry_names(Some((FAKE, "fn fake(")));
        assert_eq!(unlisted(&names), vec!["no_such_role".to_string()]);
    }
}

#[cfg(test)]
mod comment_reverse_regular_tests {
    //! 候选注释的编码反查只用常规索引（已启用词库）；「含未启用扩展词库」只给反查模式。
    //! 自造夹具，不依赖 build_dev/data：主库 `a 工`、未启用扩展 `_xz` 里 `uuia 门头沟区`。
    use crate::coordinator::Coordinator;
    use std::path::PathBuf;
    use wind_config::Config;

    struct Cleanup {
        id: String,
        dir: PathBuf,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(cache) = Config::cache_dir() {
                let _ = std::fs::remove_dir_all(cache.join(&self.id));
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn coord() -> (std::sync::Arc<Coordinator>, Cleanup) {
        let id = format!("zz_crs_on_{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("wind_crs_{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        let s = dir.join("schemas");
        std::fs::create_dir_all(s.join(&id)).unwrap();
        std::fs::write(
            s.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"注\"\n[engine]\ntype = \"codetable\"\n\
                 [engine.codetable]\nmax_code_length = 4\n\
                 [[dictionaries]]\nid = \"{id}_m\"\npath = \"{id}/m.dict.yaml\"\ntype = \"rime_codetable\"\ndefault = true\n\
                 [[dictionaries]]\nid = \"{id}_xz\"\npath = \"{id}/xz.dict.yaml\"\ntype = \"rime_codetable\"\ndefault_enabled = false\n"
            ),
        )
        .unwrap();
        let dict = |body: &str| {
            format!(
                "---\nname: d\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n{body}"
            )
        };
        std::fs::write(s.join(format!("{id}/m.dict.yaml")), dict("a\t工\t100\n")).unwrap();
        std::fs::write(
            s.join(format!("{id}/xz.dict.yaml")),
            dict("uuia\t门头沟区\t0\n"),
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec![id.clone()];
        cfg.schema.active = id.clone();
        cfg.input.reverse.lookup_disabled_dicts = true;
        (
            Coordinator::new_headless(cfg, Some(&dir)),
            Cleanup { id, dir },
        )
    }

    /// 开关开着，预热后注释仍查不到未启用库的码；启用库照常。
    #[test]
    fn prewarm_indexes_never_builds_comment_variant() {
        let (c, _g) = coord();
        c.prewarm_indexes();
        assert_eq!(
            c.engine_mgr.codetable_reverse_hint("工").as_deref(),
            Some("a")
        );
        assert_eq!(
            c.engine_mgr.codetable_reverse_hint("门头沟区").as_deref(),
            Some(""),
            "未启用库不进注释反查"
        );
    }
}
