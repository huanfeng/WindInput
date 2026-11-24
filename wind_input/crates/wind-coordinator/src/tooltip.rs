//! 候选**悬停提示**（气泡）的段渲染——`ui.tooltip.*` 的唯一消费点。
//!
//! 气泡 = 有序段列表，每段 = 段名模板 + 段内容模板，语法与变量沿用注释段（[`crate::comment`]），
//! 不另起一套。本模块只加两样注释段没有的东西：
//!
//! - 逐字求值（`each = "han" | "char"`）：段内容对每个字求值一次、每字一行，「拆字 / 拼音」
//!   合并因此只是一行模板。
//! - `promote`：按某变量是否非空把逐字行稳定分成两组（如有拆字的字在前、未收录的补在末尾）。
//!
//! 变量分层取值，先到先得：只依赖候选文本的气泡变量（`full_text` / `unicode_all`，本模块
//! 自答）→ 逐字专属变量（[`char_var`]）→ 调用方注入的候选上下文变量（`word_code` /
//! `code_source` / `debug`）→ 注释段那套词汇。逐字上下文取不到的再回落候选上下文
//! （允许但通常没有意义，不报错）。
//!
//! # 原始行与显示行
//!
//! 段内容先求出原始行，再经「单行截断（`max_chars`）→ 折行（`wrap_width`）」得到显示行
//! （[`TooltipLine`]，`raw` 指回原始行下标）。复制 / 上屏取原始行（随 [`RenderedTooltip::raw`]
//! 返回）；原始行不进要下发给 UI 的 [`TooltipDoc`]，免得每次按键都复制一份长原文。
//!
//! 解析在配置快照里做一次（[`CompiledTooltip::compile`]，随 `ConfigBundle` 重建），
//! 候选循环里只渲染。设计见 `docs/design/candidate-tooltip-sections.md`。

use crate::comment::{Template, VarValue};
use tracing::warn;
use unicode_segmentation::UnicodeSegmentation;
use wind_config::config::{
    TRUNCATION_MARK, TooltipConfig, TooltipSection as SectionConfig, is_wide_char,
};
use wind_ui_types::{StyledText, TooltipDoc, TooltipLine, TooltipSection};

/// 逐字段遍历的「汉字」口径：≥ U+3400（扩展 A 起）。与段列表引入前的气泡一致。
fn is_han(c: char) -> bool {
    c as u32 >= 0x3400
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Each {
    /// 整个候选求值一次。
    Whole,
    /// 显示文本里每个 ≥U+3400 的字。
    Han,
    /// 显示文本里每个非空白字符。
    Char,
}

#[derive(Debug, Clone)]
struct CompiledSection {
    label: Template,
    template: Template,
    each: Each,
    promote: Option<String>,
    inline: bool,
    /// 模板**字面**写了 `\t`：这段是分列行（「拆字 / 拼音」合并段），含 `\t` 的行不折。
    columns: bool,
}

/// 预解析的段列表（只含启用的段）+ 显示行的长度保护。
#[derive(Debug, Clone, Default)]
pub(crate) struct CompiledTooltip {
    sections: Vec<CompiledSection>,
    /// 单行显示上限（字符数），0 = 不限。
    max_chars: usize,
    /// 折行宽度（显示列），0 = 不折。
    wrap_width: usize,
}

/// 一个候选的气泡：给 UI 的显示结构 + 协调器自留的原始行。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RenderedTooltip {
    /// 与下发给 UI 的 `CandidateItem::tooltip` 共享同一份。
    pub(crate) doc: std::sync::Arc<TooltipDoc>,
    /// `raw[段][原始行]`，与 `doc.sections` 一一对应；`TooltipLine::raw` 是第二维下标。
    pub(crate) raw: Vec<Vec<String>>,
    /// 每段是否逐字段（`each = han | char`），与 `doc.sections` 一一对应。右键菜单只对
    /// 逐字段给「复制此行 / 上屏此行」：整段求值的多行（调试信息、多段落原文）不是
    /// 彼此独立的条目。
    pub(crate) per_char: Vec<bool>,
}

impl RenderedTooltip {
    /// 第 `section` 段的原始内容（原始行以 `\n` 连接，不含段名）。越界为 `None`。
    pub(crate) fn section_text(&self, section: usize) -> Option<String> {
        self.raw.get(section).map(|lines| lines.join("\n"))
    }

    /// 第 `section` 段第 `line` 条原始行。越界为 `None`。
    pub(crate) fn line_text(&self, section: usize, line: usize) -> Option<&str> {
        self.raw.get(section)?.get(line).map(String::as_str)
    }

    /// 「复制全部」：段落格式照 `TooltipDoc::to_plain_text`（`[段名]` 独占一行、inline 段
    /// 写成 `段名: 内容`、段间换行），但内容取**原始行**——完整原文不带截断的 `…`、
    /// 也不带折行插进去的换行。复制出去的是信息，不是气泡的排版。
    pub(crate) fn raw_plain_text(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        for (sec, raw) in self.doc.sections.iter().zip(&self.raw) {
            match (&sec.title, raw.as_slice()) {
                (Some(t), [only]) if sec.inline => out.push(format!("{}: {only}", t.as_str())),
                (title, lines) => {
                    if let Some(t) = title {
                        out.push(format!("[{}]", t.as_str()));
                    }
                    out.extend(lines.iter().cloned());
                }
            }
        }
        out.join("\n")
    }
}

impl CompiledTooltip {
    /// `ui.tooltip.enabled = false` 编译出**空段列表**：这是悬停总开关的唯一落点——候选循环
    /// 因此不渲染任何段、不查编码（`references` 全否），`DataNeeds` 里悬停的贡献也随之归零。
    pub(crate) fn compile(cfg: &TooltipConfig) -> Self {
        let sections = cfg
            .sections
            .iter()
            .filter(|s| cfg.enabled && s.enabled)
            .map(|s: &SectionConfig| CompiledSection {
                label: Template::parse(&s.label),
                template: Template::parse(&s.template),
                each: match s.each.trim() {
                    "" => Each::Whole,
                    "han" => Each::Han,
                    "char" => Each::Char,
                    // 写错不静默：按整段求值照常出内容，但日志里留下线索。
                    other => {
                        warn!("ui.tooltip.sections: 未知的 each = {other:?}，按整段求值处理");
                        Each::Whole
                    }
                },
                promote: Some(s.promote.trim())
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
                inline: s.inline,
                columns: Template::parse(&s.template).has_literal('\t'),
            })
            .collect();
        Self {
            sections,
            max_chars: cfg.max_chars,
            wrap_width: cfg.wrap_width,
        }
    }

    /// 启用的段里是否引用了变量 `name`（段名、内容、`promote` 任一处）。调用方据此决定要不要
    /// 准备代价高的数据：调试上下文、编码反查索引——没有段用到就一次也不碰。
    pub(crate) fn references(&self, name: &str) -> bool {
        self.sections.iter().any(|s| {
            s.label.references(name)
                || s.template.references(name)
                || s.promote.as_deref() == Some(name)
        })
    }

    /// 按段列表渲染一个候选的气泡。有任一非空段就有气泡，不看候选是不是汉字。
    ///
    /// - `disp` / `full`：候选的显示文本（按 `ui.candidate.max_chars` 截断后）与完整原文。
    ///   两者不同即「被截断了」：`${full_text}` 此时才有值。逐字段遍历的是 `disp` 去掉
    ///   截断标记 `…` 的部分——那个 `…` 不是候选的字，不该出一行 Unicode。规模因此受
    ///   `ui.candidate.max_chars` 控制，完整原文不会让逐字段展开成几十行。
    /// - `cand`：候选上下文求值，`None` = 未知变量名（渲染层原样回显，拼错看得见）。
    /// - `per_char`：逐字上下文求值；返回 `None` 时回落候选上下文。
    pub(crate) fn render(
        &self,
        disp: &str,
        full: &str,
        cand: &impl Fn(&str, Option<&str>) -> Option<VarValue>,
        per_char: &impl Fn(char, &str, Option<&str>) -> Option<VarValue>,
    ) -> RenderedTooltip {
        let truncated = disp != full;
        let shown = if truncated {
            disp.strip_suffix(TRUNCATION_MARK).unwrap_or(disp)
        } else {
            disp
        };
        let cand_eval = |name: &str, arg: Option<&str>| -> Option<VarValue> {
            match name {
                "full_text" => Some(VarValue::from(if truncated {
                    full.to_string()
                } else {
                    String::new()
                })),
                // `${unicode_all[:分隔符]}` —— 逐字码位（跳过空白），默认空格连接。
                "unicode_all" => Some(VarValue::joined(
                    shown.chars().filter(|c| !c.is_whitespace()).map(unicode_of),
                    arg.unwrap_or(" "),
                )),
                _ => cand(name, arg),
            }
        };
        let mut out = RenderedTooltip::default();
        for sec in &self.sections {
            let rows = match sec.each {
                Each::Whole => {
                    let (text, filled) = sec.template.render_styled(&cand_eval, &|_| true, false);
                    if filled { vec![text] } else { Vec::new() }
                }
                Each::Han | Each::Char => {
                    let chars = shown.chars().filter(|&c| match sec.each {
                        Each::Han => is_han(c),
                        _ => !c.is_whitespace(),
                    });
                    let mut rows: Vec<(bool, StyledText)> = Vec::new();
                    for c in chars {
                        let eval = |name: &str, arg: Option<&str>| {
                            per_char(c, name, arg).or_else(|| cand_eval(name, arg))
                        };
                        // `${char}` 恒非空，不能让它撑起一行：查不到读音的字不该剩下 `好：`。
                        let (text, filled) =
                            sec.template.render_styled(&eval, &|n| n != "char", false);
                        if !filled {
                            continue;
                        }
                        let promoted = sec
                            .promote
                            .as_deref()
                            .is_some_and(|p| eval(p, None).is_some_and(|v| !v.is_empty()));
                        rows.push((promoted, text));
                    }
                    // 稳定分组：非空组在前，组内保持原文顺序。
                    if sec.promote.is_some() {
                        rows.sort_by_key(|(promoted, _)| !promoted);
                    }
                    rows.into_iter().map(|(_, text)| text).collect()
                }
            };
            // 原始行是复制 / 上屏的取值来源，必须保真：不 trim、保留段内空行（多段落原文），
            // 只去掉首尾的空白行；全是空白即空段。样式跟着行走：原始行存纯文本（复制 / 上屏），
            // 同形的带样式行供显示。
            let mut styled: Vec<StyledText> = rows.into_iter().flat_map(split_lines).collect();
            while styled.last().is_some_and(|l| l.as_str().trim().is_empty()) {
                styled.pop();
            }
            let head = styled
                .iter()
                .take_while(|l| l.as_str().trim().is_empty())
                .count();
            styled.drain(..head);
            if styled.is_empty() {
                continue;
            }
            let raw: Vec<String> = styled.iter().map(|l| l.as_str().to_string()).collect();
            // 段名的字面文字不随变量全空而消失：`编码{(${code_source})}` 直接输入时就是 `编码`。
            let (title, _) = sec.label.render_styled(&cand_eval, &|_| true, true);
            let title = title.trim();
            // inline 按**原始行数**判：一条长原文折成多条显示行仍是「一行内容」，照样写成
            // `标题: 内容`；此时 `标题: ` 占掉第一条显示行的宽度。
            let inline = sec.inline && raw.len() == 1 && !title.is_empty();
            let prefix = if inline {
                str_width(title.as_str()) + 2
            } else {
                0
            };
            let lines = styled
                .into_iter()
                .enumerate()
                .flat_map(|(i, l)| {
                    let idx = u16::try_from(i).unwrap_or(u16::MAX);
                    self.display_lines(l, sec.columns, if i == 0 { prefix } else { 0 })
                        .into_iter()
                        .map(move |text| TooltipLine { text, raw: idx })
                })
                .collect();
            std::sync::Arc::make_mut(&mut out.doc)
                .sections
                .push(TooltipSection {
                    title: (!title.is_empty()).then_some(title),
                    inline,
                    lines,
                });
            out.raw.push(raw);
            out.per_char.push(sec.each != Each::Whole);
        }
        out
    }

    /// 原始行 → 显示行：先按 `max_chars` 截断（超出加 `…`），再按 `wrap_width` 折行，
    /// 丢掉全是空白的显示行。都按**字素簇**计，不会把 emoji 序列、组合字符从中间切开。
    /// 样式跟着字走：截断的 `…` 继承被截处前一簇的样式，折行按字节区间切带样式的行。
    ///
    /// - `columns`：分列段（模板字面写了 `\t`）里含 `\t` 的行不折——渲染端按 `\t` 列对齐，
    ///   从中间折开会把第二列甩到下一行行首。变量值带进来的 `\t` 只是内容，照常折。
    /// - `first_offset`：第一条显示行已被占掉的宽度（inline 段的 `标题: `）。
    fn display_lines(
        &self,
        raw: StyledText,
        columns: bool,
        first_offset: usize,
    ) -> Vec<StyledText> {
        // DirectWrite 除 `\n` 外还在 `\r`、U+0085、U+2028、U+2029 处断行。不先归一，这些字符
        // 会在渲染时多折出行来，而行数是命中换算的前提（每行等高、按 `\n` 计行）——点第 3 行
        // 会命中第 2 行。只改显示行；原始行（复制 / 上屏的取值）保留原字符。
        let normalized = normalize_breaks(raw);
        let mut out = Vec::new();
        for (i, part) in split_lines(normalized).into_iter().enumerate() {
            let line = match part.as_str().grapheme_indices(true).nth(self.max_chars) {
                Some((cut, _)) if self.max_chars > 0 => {
                    part.cut_with_mark(cut, TRUNCATION_MARK.encode_utf8(&mut [0; 4]))
                }
                _ => part,
            };
            if self.wrap_width == 0 || (columns && line.as_str().contains('\t')) {
                out.push(line);
            } else {
                let ranges = wrap(
                    line.as_str(),
                    self.wrap_width,
                    if i == 0 { first_offset } else { 0 },
                );
                // 常见情形一行放得下：原样搬走，不切片、不分配。
                if let [(0, e)] = ranges.as_slice()
                    && *e == line.len()
                {
                    out.push(line);
                } else {
                    out.extend(ranges.into_iter().map(|(s, e)| line.slice(s, e)));
                }
            }
        }
        out.retain(|l| !l.as_str().trim().is_empty());
        out
    }
}

/// 按 `\n` 切成多行（带样式），同 `str::split('\n')`。没有换行时原样返回、不分配。
fn split_lines(t: StyledText) -> Vec<StyledText> {
    if !t.as_str().contains('\n') {
        return vec![t];
    }
    let s = t.as_str();
    let mut out = Vec::new();
    let mut at = 0usize;
    for (i, _) in s.match_indices('\n') {
        out.push(t.slice(at, i));
        at = i + 1;
    }
    out.push(t.slice(at, s.len()));
    out
}

/// 断行符归一为 `\n`：`\r\n` 本就是一个字素簇，逐簇映射，样式跟着该簇走。
fn normalize_breaks(t: StyledText) -> StyledText {
    let is_break = |c: char| matches!(c, '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
    if !t.as_str().contains(is_break) {
        return t;
    }
    let mut out = StyledText::new();
    for (i, g) in t.as_str().grapheme_indices(true) {
        let g = if g == "\r\n" || g.chars().all(is_break) {
            "\n"
        } else {
            g
        };
        out.push(g, &t.style_at(i));
    }
    out
}

/// 字素簇的显示宽度：CJK / 全角记 2（口径同 `is_wide_char`），emoji 也记 2——含 ZWJ 或
/// 变体选择符 U+FE0F 的序列、以及补充平面的 emoji 区。其余记 1。
///
/// 与 `is_wide_char` 刻意不同：那条口径给图标主字用，把 emoji 记 1 是为了 C++ 侧缓冲容量
/// （见其文档）；气泡是在量一行摆不摆得下，emoji 在屏上就是两列宽。
fn grapheme_width(g: &str) -> usize {
    let first = g.chars().next().unwrap_or(' ');
    let emoji = matches!(first as u32, 0x1F000..=0x1FAFF)
        || g.contains('\u{200D}')
        || g.contains('\u{FE0F}');
    if is_wide_char(first) || emoji { 2 } else { 1 }
}

fn str_width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// 连续 ASCII 片段里的断点字符：断在它**之后**（`a/ab/` | `abc`）。
fn is_break_char(g: &str) -> bool {
    matches!(g, " " | "/" | "·")
}

/// 按显示宽度折行，返回各显示行在 `line` 里的字节区间（样式由调用方按区间切）。
/// 溢出发生在一段连续 ASCII（编码列表 `a/ab/abc`、英文单词）中间时，
/// 优先退回到这段里最后一个空格、`/`、`·` 之后断开，把被切的 token 整个带到下一行；
/// 这段里没有断点（或溢出点不在 ASCII 里，如汉字）才硬折。
///
/// 当前行为空时不折：宽度不足一个全角字（`width = 1`）时，那个字独占一行而不是死循环。
fn wrap(line: &str, width: usize, first_offset: usize) -> Vec<(usize, usize)> {
    // 一段连续字素簇 → 字节区间（去掉尾部空白）。
    let range = |cur: &[(usize, &str, usize)], trim: bool| -> (usize, usize) {
        let start = cur[0].0;
        let end = cur.last().map_or(start, |(o, g, _)| o + g.len());
        if trim {
            (start, start + line[start..end].trim_end().len())
        } else {
            (start, end)
        }
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut cur: Vec<(usize, &str, usize)> = Vec::new();
    let mut w = 0usize;
    for (off, g) in line.grapheme_indices(true) {
        // 折出来的续行不以空白开头：那串空白就是断点（第一行的缩进是内容，照留）。
        if cur.is_empty() && !out.is_empty() && g.trim().is_empty() {
            continue;
        }
        let gw = grapheme_width(g);
        let limit = if out.is_empty() {
            width.saturating_sub(first_offset).max(1)
        } else {
            width
        };
        if w + gw > limit && !cur.is_empty() {
            let token_char = g.is_ascii() && !g.trim().is_empty() && !is_break_char(g);
            let carry = if token_char {
                last_ascii_break(&cur).map(|at| cur.split_off(at))
            } else {
                None
            };
            out.push(range(&cur, true));
            cur = carry.unwrap_or_default();
            w = cur.iter().map(|(_, _, w)| w).sum();
            if g.trim().is_empty() && cur.is_empty() {
                continue;
            }
        }
        cur.push((off, g, gw));
        w += gw;
    }
    if !cur.is_empty() {
        out.push(range(&cur, false));
    }
    out
}

/// `cur` 末尾那段连续 ASCII 里最后一个断点之后的位置；末尾不是 ASCII token、或这段里
/// 没有断点、或断点就在末尾（等于没有可带走的部分）时返回 `None`。
fn last_ascii_break(cur: &[(usize, &str, usize)]) -> Option<usize> {
    for (i, (_, g, _)) in cur.iter().enumerate().rev() {
        if is_break_char(g) {
            return (i + 1 < cur.len() && i > 0).then_some(i + 1);
        }
        if !g.is_ascii() || g.trim().is_empty() {
            return None;
        }
    }
    None
}

/// [`char_var`] 认得的变量名（与 match 分支一一对应，见 comment.rs 的 `EVAL_VAR_NAMES` 一节）。
pub(crate) const CHAR_VAR_NAMES: &[&str] = &["readings", "unicode"];

/// 气泡整段求值（`TooltipSections::render` 里的 `cand_eval`）自己认得的变量名。
pub(crate) const TOOLTIP_CAND_VAR_NAMES: &[&str] = &["full_text", "unicode_all"];

/// 协调器给气泡的候选级求值（coordinator.rs 候选循环里的 `cand_eval`）自己认得的变量名，
/// 其余回落 `eval_var`。
pub(crate) const TOOLTIP_COORD_VAR_NAMES: &[&str] = &["word_code", "code_source", "debug"];

/// 逐字上下文里气泡专属的变量。`None` = 不是这里的变量（交给下一层）。
///
/// - `readings[:N]` —— 该字全部读音（最常用在前）以 `/` 连接，`N` 限前 N 个；`N` 不是正整数时按不限。
/// - `unicode` —— 码位，`U+597D`；非 BMP 照写五 / 六位（`U+2A6D6`）。
pub(crate) fn char_var(
    name: &str,
    arg: Option<&str>,
    c: char,
    reverse: &wind_reverse::ReverseLookup,
) -> Option<String> {
    Some(match name {
        "readings" => {
            let max = arg
                .and_then(|a| a.trim().parse::<usize>().ok())
                .unwrap_or(0);
            reverse.readings_of(c, max, "/")
        }
        "unicode" => unicode_of(c),
        _ => return None,
    })
}

fn unicode_of(c: char) -> String {
    format!("U+{:04X}", c as u32)
}

#[cfg(test)]
#[path = "tooltip_tests.rs"]
mod tests;
