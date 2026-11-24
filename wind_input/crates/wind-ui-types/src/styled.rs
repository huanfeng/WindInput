//! 带分段样式的文字（设计 `docs/design/text-span-colors.md` §3）。
//!
//! 候选注释与悬停提示的文字由模板拼成，每个变量值、每段字面文字各自带一个**角色**（变量名或
//! `title` / `literal`）或内联色；UI 按当前主题把它们解析成颜色（`wind_theme::span_color`）。

use std::sync::Arc;

use smallvec::SmallVec;

pub use wind_theme::InlineColor;

/// 对外契约清单：模板里能产出的**全部角色名**（变量名 + 结构角色）。
///
/// 文档站角色表与主题编辑器严格模式照它列出与校验。**引擎也查它**：片段的角色取自这张表里的
/// 常驻字符串（[`static_role`]，性能原因见 [`Span::role`]），清单外的变量不产角色、主题给它配的色
/// 不生效（模板引擎对此 warn 一次）。主题里写了清单外的角色名则静默忽略——更新版本的主题在旧
/// 引擎上不该报错。
///
/// 新增模板变量时必须同步加进来。两个方向都有守卫，都在 wind-coordinator：清单里每个名字都能被
/// 某个求值入口求出值（`every_text_role_is_evaluable_somewhere`）；各求值入口 `match` 里的每个
/// 变量名都在清单里（`every_evaluable_variable_is_a_listed_role`，扫源码）。
pub const TEXT_ROLES: &[&str] = &[
    // 候选上下文（`Coordinator::eval_var`）
    "code_hint",
    "emoji",
    "code_rev",
    "code_rev_all",
    "shuangpin",
    "pinyin",
    "correct_pinyin",
    "chaizi",
    "chaizi_code",
    "chaizi_all",
    "chaizi_code_all",
    "dict",
    // 气泡候选上下文
    "word_code",
    "code_source",
    "debug",
    "full_text",
    "unicode_all",
    // 逐字上下文
    "readings",
    "unicode",
    "char",
    // 结构角色：模板字面文字（正文 / 段名里）
    "title",
    "literal",
    // 结构角色：`${name:分隔符}` 一类变量的分项分隔符（片段带 `separator` 标记、角色仍是变量名；
    // 主题没配它时随变量同色，见 `wind_theme::span_color`）
    "separator",
    // 结构角色：候选正文里内嵌的模糊音正确拼音（「苹(ping)果」的 `(ping)`），协调器直接产出，
    // 不是模板变量
    "pinyin_hint",
];

/// [`TEXT_ROLES`] 里的结构角色：不是模板变量、没有求值入口。两条守卫与注释预览按它豁免。
pub const STRUCTURAL_ROLES: &[&str] = &["title", "literal", "separator", "pinyin_hint"];

/// 清单里的同名常驻字符串（`Span::role` 用）；清单外为 `None`。
pub fn static_role(name: &str) -> Option<&'static str> {
    TEXT_ROLES.iter().copied().find(|r| *r == name)
}

/// 带样式文字里的一段：`[start, end)` 字节区间（落在字符边界上）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    /// 角色 = 归一后的变量名，或结构角色 `title` / `literal`；`None` = 无角色（未知变量回显）。
    ///
    /// 取自 [`TEXT_ROLES`] 的常驻字符串（[`static_role`]）：每个变量值都要带角色，候选循环里
    /// 逐片段分配或引用计数会在满页 × 每次按键的频率上显出来（设计 §13.3 的 < 5% 预算）。
    /// 清单外的名字没有角色、按正文色——主题本就无从给一个没公开的名字配色；新增模板变量时
    /// 连同清单一起加。
    pub role: Option<&'static str>,
    /// 在段名（气泡 label）里产出的片段：角色未配色时回落 `title` 角色，而非正文色。
    pub in_title: bool,
    /// 变量值里的分项分隔符（`${chaizi_all:／}` 的 `／`）：主题配了 `separator` 角色就用它，
    /// 没配则按 `role`（所在变量）求色。
    pub separator: bool,
    /// 内联色（`$[…]{}`）。有值即优先于角色。共享持有：一个 `$[…]{}` 里的全部片段指向同一份。
    pub color: Option<Arc<InlineColor>>,
}

impl Span {
    fn same_style(&self, style: &SpanStyle) -> bool {
        self.role == style.role
            && self.in_title == style.in_title
            && self.separator == style.separator
            && self.color == style.color
    }
}

/// 一段文字的样式（[`StyledText::push`] 的参数、[`StyledText::style_at`] 的结果）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    pub role: Option<&'static str>,
    pub in_title: bool,
    pub separator: bool,
    pub color: Option<Arc<InlineColor>>,
}

impl SpanStyle {
    /// 没有任何可着色的信息：无角色、无内联色（`in_title` 单独不足以着色——它只是角色的回落
    /// 方向，§3.2 未知变量回显在段名里也不回落 title）。这样的文字不建区间，按正文色。
    fn is_plain(&self) -> bool {
        self.role.is_none() && self.color.is_none() && !self.separator
    }
}

/// 带分段样式的文字：纯文本 + 挂在旁边的区间表。
///
/// 下游绝大多数代码只关心文字（测量、截断判定、复制、上屏、右键菜单标签），读 [`Self::as_str`]；
/// 只有画字的那一处读区间。文字与区间捆在一个类型里、`text` 私有：并列字段或公开 `text` 都会
/// 出现「文字改了、区间没跟着改」——截断、trim、吞空白都在改文字。改文字只能走这里的方法，
/// 不变量（升序、不重叠、在界内、在字符边界）只守一处。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct StyledText {
    text: String,
    /// 按 `start` 升序、互不重叠、非空。未被覆盖的文字 = 节点正文色。
    ///
    /// 内联存放前 3 段：出厂模板下一段注释 / 一条气泡行只有一两个片段，免得每条文字一次堆分配、
    /// 气泡文档整份克隆（下发 UI 一份、右键菜单留一份）时再一次（§13.3 的 < 5% 预算，实测）。
    spans: SmallVec<[Span; 3]>,
}

impl From<String> for StyledText {
    fn from(text: String) -> Self {
        Self {
            text,
            spans: SmallVec::new(),
        }
    }
}

impl From<&str> for StyledText {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}

impl StyledText {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    fn spans_mut(&mut self) -> &mut SmallVec<[Span; 3]> {
        &mut self.spans
    }

    pub fn into_string(self) -> String {
        self.text
    }

    /// 追加一段文字。与末尾区间同样式且相邻时合并，免得逐字追加时区间碎成一地。
    pub fn push(&mut self, s: &str, style: &SpanStyle) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len() as u32;
        self.text.push_str(s);
        let end = self.text.len() as u32;
        if style.is_plain() {
            return;
        }
        let spans = self.spans_mut();
        if let Some(last) = spans.last_mut()
            && last.end == start
            && last.same_style(style)
        {
            last.end = end;
            return;
        }
        spans.push(Span {
            start,
            end,
            role: style.role,
            in_title: style.in_title,
            separator: style.separator,
            color: style.color.clone(),
        });
    }

    /// 追加另一段带样式文字（区间按偏移并入）。
    pub fn append(&mut self, other: &StyledText) {
        let mut at = 0usize;
        for sp in other.spans() {
            let (s, e) = (sp.start as usize, sp.end as usize);
            if at < s {
                self.push(&other.text[at..s], &SpanStyle::default());
            }
            self.push(
                &other.text[s..e],
                &SpanStyle {
                    role: sp.role,
                    in_title: sp.in_title,
                    separator: sp.separator,
                    color: sp.color.clone(),
                },
            );
            at = e;
        }
        if at < other.text.len() {
            self.push(&other.text[at..], &SpanStyle::default());
        }
    }

    /// 末尾字符满足 `pred` 就删掉它（并收缩 / 删除末尾区间）。模板引擎「空变量吞掉紧邻的
    /// 一个空白」用它：吞的永远是**同一个**输出缓冲的末尾，跨不跨颜色边界都一样。
    pub fn pop_if(&mut self, pred: impl Fn(char) -> bool) -> bool {
        let Some(c) = self.text.chars().next_back() else {
            return false;
        };
        if !pred(c) {
            return false;
        }
        self.text.pop();
        let end = self.text.len() as u32;
        if self.spans().last().is_some_and(|l| l.end > end) {
            let spans = self.spans_mut();
            let last = spans.last_mut().expect("刚判过非空");
            last.end = end;
            if last.start >= last.end {
                spans.pop();
            }
        }
        true
    }

    /// 取 `[start, end)` 字节区间（须在字符边界上），区间裁剪并平移。
    pub fn slice(&self, start: usize, end: usize) -> StyledText {
        let (s, e) = (start as u32, end as u32);
        let spans: SmallVec<[Span; 3]> = self
            .spans()
            .iter()
            .filter_map(|sp| {
                let (a, b) = (sp.start.max(s), sp.end.min(e));
                (a < b).then(|| Span {
                    start: a - s,
                    end: b - s,
                    ..sp.clone()
                })
            })
            .collect();
        StyledText {
            text: self.text[start..end].to_string(),
            spans,
        }
    }

    /// 字节位置 `at` 处那个字的样式（不在任何区间内 = 默认样式）。
    pub fn style_at(&self, at: usize) -> SpanStyle {
        let at = at as u32;
        self.spans()
            .iter()
            .find(|sp| sp.start <= at && at < sp.end)
            .map(|sp| SpanStyle {
                role: sp.role,
                in_title: sp.in_title,
                separator: sp.separator,
                color: sp.color.clone(),
            })
            .unwrap_or_default()
    }

    /// 去掉首尾空白（Unicode 空白，同 `str::trim`）。
    pub fn trim(&self) -> StyledText {
        self.clone().into_trimmed()
    }

    /// 同 [`Self::trim`]，就地处理：两端本就没有空白时原样返回、不分配（模板渲染的热路径）。
    pub fn into_trimmed(self) -> StyledText {
        let t = self.text.trim_start();
        let start = self.text.len() - t.len();
        let end = start + t.trim_end().len();
        if start == 0 && end == self.text.len() {
            return self;
        }
        self.slice(start, end)
    }

    /// 按字符数截断：超过 `max_chars` 个字就只留前 `max_chars` 个，再接 `mark`。`mark` 继承被截处
    /// 前一个字的样式——它不承载信息，不单设角色。`max_chars == 0` 表示不限。
    pub fn truncate_chars(&self, max_chars: usize, mark: &str) -> StyledText {
        self.clone().into_truncated(max_chars, mark)
    }

    /// 同 [`Self::truncate_chars`]，没超出时原样返回、不分配。
    pub fn into_truncated(self, max_chars: usize, mark: &str) -> StyledText {
        if max_chars == 0 {
            return self;
        }
        match self.text.char_indices().nth(max_chars) {
            Some((cut, _)) => self.cut_with_mark(cut, mark),
            None => self,
        }
    }

    /// 截在字节位置 `cut`（字符边界），接上继承前一个字样式的 `mark`。
    pub fn cut_with_mark(&self, cut: usize, mark: &str) -> StyledText {
        let mut out = self.slice(0, cut);
        let prev = self.text[..cut]
            .char_indices()
            .next_back()
            .map(|(i, _)| self.style_at(i))
            .unwrap_or_default();
        out.push(mark, &prev);
        out
    }

    /// 把每个 `from` 换成 `to`。两者须同为 ASCII（单字节），字节长度不变，区间因而原样有效。
    pub fn replace_ascii(mut self, from: char, to: char) -> StyledText {
        debug_assert!(
            from.is_ascii() && to.is_ascii(),
            "replace_ascii 只接受 ASCII"
        );
        if self.text.contains(from) {
            self.text = self.text.replace(from, to.encode_utf8(&mut [0; 1]));
        }
        self
    }

    /// 以 `sep` 连接多段。
    pub fn join(parts: &[StyledText], sep: &str) -> StyledText {
        let mut out = StyledText::new();
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                out.push(sep, &SpanStyle::default());
            }
            out.append(p);
        }
        out
    }
}

#[cfg(test)]
#[path = "styled_tests.rs"]
mod tests;
