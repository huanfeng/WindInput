//! 候选词条数据。

use crate::styled::{SpanStyle, StyledText};

/// 候选词数据
#[derive(Debug, Clone)]
pub struct CandidateItem {
    pub text: String,
    pub code: String,
    /// 序号标签（如 "1" / "a"）；空则按位置自动用数字编号
    pub label: String,
    /// 悬停提示（结构化，段 → 显示行）。空文档 = 不显示气泡。
    ///
    /// 共享持有：协调器的右键菜单缓存与下发给 UI 的这份是同一个文档，UI 悬停时再取也不深拷贝
    /// ——每次按键满页候选各一份，带样式的文字深拷贝一遍在 §13.3 的预算里显得出来。
    pub tooltip: std::sync::Arc<TooltipDoc>,
    /// 候选注释（编码后缀/短语提示等），非空时在候选词右侧以注释样式内联显示；空则不显示。
    /// 模板渲染出的注释带分段样式（角色 / 内联色）；其余来源 `String::into()` 即无样式。
    pub comment: StyledText,
    /// 上方注释条（`ui.candidate.comment_above` 开启且模板含字面 `\n` 时的上段）；空 = 无上方条。
    pub comment_above: StyledText,
    /// 为 true 时完全不渲染序号节点（用于非候选的提示行，如快捷加词预览），
    /// 避免默认主题下出现空的序号圆圈。
    pub no_index: bool,
}

/// 候选展开态（多行网格）的版式，见 `docs/design/candidate-grid-expand.md`。
///
/// 网格的**一行就是原来的一页**：`candidates` 是可见区内逐行平铺的候选，第 `r` 行第 `c` 列
/// 在 `r * cols + c`。`selected` / `hover` 与命中矩形的 tag 都是这个平铺数组的下标，
/// 宿主照旧原样回传，换算回「哪一页、页内第几个」是协调器的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateGrid {
    /// 每行候选数（= 每页候选数）。
    pub cols: u16,
    /// 可见行数。末屏行数不足时渲染端补等高占位行，保证滚动期间窗口高度不变。
    pub rows: u16,
    /// 高亮行在可见区内的行号（0 起）。
    pub highlight_row: u16,
    /// 显示序号的格子：平铺下标 `index_start..index_start + index_count`（从高亮行行首起，
    /// 按 `ui.candidate.grid_index_fill` 往下顺延）。数字键选的就是这几格，其余格子的序号
    /// 渲染端画成透明占位。
    pub index_start: u16,
    pub index_count: u16,
    /// 分类标签栏：可用标签的位集（`1 << GridTab as u8`）；0 = 不显示标签栏。
    pub tabs: u8,
    /// 当前分类（`GridTab as u8`）。
    pub active_tab: u8,
}

/// 展开态的候选分类（标签栏）。枚举顺序即标签栏从左到右的顺序。
///
/// 判据在协调器（`candidate_grid`），这里只有名字与编号的纯映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum GridTab {
    #[default]
    All = 0,
    Single = 1,
    Phrase = 2,
    Rare = 3,
    Symbol = 4,
}

impl GridTab {
    pub const ALL: [GridTab; 5] = [
        GridTab::All,
        GridTab::Single,
        GridTab::Phrase,
        GridTab::Rare,
        GridTab::Symbol,
    ];

    /// 标签栏上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            GridTab::All => "全部",
            GridTab::Single => "单字",
            GridTab::Phrase => "词组",
            GridTab::Rare => "生僻",
            GridTab::Symbol => "符号",
        }
    }

    /// 配置里的写法（`ui.candidate.grid_categories`）。
    pub fn key(self) -> &'static str {
        match self {
            GridTab::All => "all",
            GridTab::Single => "single",
            GridTab::Phrase => "phrase",
            GridTab::Rare => "rare",
            GridTab::Symbol => "symbol",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    pub fn from_index(i: u8) -> Option<Self> {
        Self::ALL.get(i as usize).copied()
    }

    /// 在 [`CandidateGrid::tabs`] 位集里的那一位。
    pub fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// 分类标签在命中矩形里的 tag：`GRID_TAB_TAG_BASE + GridTab as u8`。
///
/// 取一个远大于网格格数（可见行 ≤ 10、每页 ≤ 10）的**正数**段：宿主只把翻页键重映射成负数，
/// 其余命中下标一律当候选下标原样回传，于是标签点击走候选点选那条路回到协调器，三端宿主
/// 不用改。悬停 / 右键落在这一段时，协调器的范围校验把它当越界忽略。
pub const GRID_TAB_TAG_BASE: i32 = 1000;

/// 结构化的悬停提示：有序段列表。纯文本形态见 [`TooltipDoc::to_plain_text`]。
///
/// 段结构一路保留到 UI，右键才有东西可命中（按段 / 按行复制、上屏）：渲染端按
/// [`Self::to_plain_text`] 画出整块文本，右键时用 [`Self::hit_at_line`] 把点中的那一行
/// 换算回 `(段, 原始行)`。只下发**显示行**；原始行（复制 / 上屏的取值）留在协调器。
/// 设计见 `docs/design/candidate-tooltip-sections.md` §6、§7。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TooltipDoc {
    pub sections: Vec<TooltipSection>,
}

/// 气泡的一段。只有非空段才会进 [`TooltipDoc`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TooltipSection {
    /// 已求值的段名；`None` = 无标题行。
    pub title: Option<StyledText>,
    /// 渲染成 `标题: 第一条显示行`（其余显示行照常另起），而非标题独占一行。
    ///
    /// 由协调器按**原始内容恰为一行**判定后置位：一条长内容折成多条显示行仍算一行，
    /// 故这里不再看 `lines.len()`。
    pub inline: bool,
    /// 非空。
    pub lines: Vec<TooltipLine>,
}

/// 气泡的一条显示行。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TooltipLine {
    pub text: StyledText,
    /// 所属原始行在本段中的下标。现在一条原始行恰对应一条显示行；将来单行截断 / 折行
    /// 会把一条原始行拆成多条显示行，它们指向同一个下标，复制 / 上屏据此取回原文。
    pub raw: u16,
}

/// 右键点中气泡的位置：第几段（`sections` 下标），以及该段的第几条**原始行**
/// （点在标题行上为 `None`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooltipHit {
    pub section: u16,
    pub raw_line: Option<u16>,
}

impl TooltipDoc {
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// 内容指纹。右键时 UI 带上它，协调器据此核对「UI 画的」与「缓存里的」是不是同一份：
    /// 同一个候选的气泡可能在菜单弹出前刷新过（如反查索引后台建好，前面多出一段），
    /// 段下标一错位，「上屏此行」就会取到别的段。
    ///
    /// 只 hash **文字与 `raw` 下标**（段名文字、inline、每行文字与 `raw`），不含分段样式：
    /// 右键核对防的是段下标错位，只与文字结构有关。颜色也进指纹的话，只改了颜色的刷新
    /// （设置页改了模板里的 `$[…]`）会让菜单弹出前后指纹不等、菜单退化成只剩「截图此窗口」，
    /// 而此时取值其实完全正确。「含颜色都相同」直接比 `PartialEq`。
    ///
    /// `DefaultHasher::new()` 的键是固定的，同一进程内结果稳定——UI 与协调器同进程，够用。
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.sections.len().hash(&mut h);
        for sec in &self.sections {
            sec.title.as_ref().map(StyledText::as_str).hash(&mut h);
            sec.inline.hash(&mut h);
            sec.lines.len().hash(&mut h);
            for l in &sec.lines {
                l.text.as_str().hash(&mut h);
                l.raw.hash(&mut h);
            }
        }
        h.finish()
    }

    /// 纯文本的逐行形态，每行带上它命中的位置。[`Self::to_plain_text`] 与
    /// [`Self::hit_at_line`] 都从这里取——画出来的行与命中换算的行必须是同一份排列。
    ///
    /// inline 段的首行既是标题也是内容，按内容算（命中该段第一条原始行）。
    ///
    /// 段名的装饰 `[` `]` 与 inline 的 `: ` 由这里拼出、不是模板字面，带 `title` 角色
    /// （设计 text-span-colors.md §3.2）：主题给段名配色时，括号跟段名同色。
    fn plain_lines(&self) -> Vec<(StyledText, TooltipHit)> {
        let deco = SpanStyle {
            role: Some("title"),
            in_title: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        for (si, sec) in self.sections.iter().enumerate() {
            let section = u16::try_from(si).unwrap_or(u16::MAX);
            let hit = |raw| TooltipHit {
                section,
                raw_line: raw,
            };
            match (&sec.title, sec.lines.as_slice()) {
                (Some(t), [first, rest @ ..]) if sec.inline => {
                    let mut line = t.clone();
                    line.push(": ", &deco);
                    line.append(&first.text);
                    out.push((line, hit(Some(first.raw))));
                    out.extend(rest.iter().map(|l| (l.text.clone(), hit(Some(l.raw)))));
                }
                (title, lines) => {
                    if let Some(t) = title {
                        let mut line = StyledText::new();
                        line.push("[", &deco);
                        line.append(t);
                        line.push("]", &deco);
                        out.push((line, hit(None)));
                    }
                    out.extend(lines.iter().map(|l| (l.text.clone(), hit(Some(l.raw)))));
                }
            }
        }
        out
    }

    /// 纯文本第 `line` 行（从 0 起，按 `\n` 切）命中哪里；越界为 `None`。
    pub fn hit_at_line(&self, line: usize) -> Option<TooltipHit> {
        self.plain_lines().get(line).map(|(_, h)| *h)
    }

    /// 纯文本形态：段间换行；有标题的段写成 `[标题]` 独占一行再逐行列内容，
    /// `inline` 段写成 `标题: 第一行`、其余行照列；无标题的段只列内容。
    ///
    /// 这是气泡、「复制全部」与 macOS 下发共用的格式，与段列表引入前的输出逐字节相同。
    pub fn to_plain_text(&self) -> String {
        self.to_styled().into_string()
    }

    /// 与 [`Self::to_plain_text`] 同一份排列的带样式整块文字（自绘气泡与 macOS 下发用）。
    /// 画出来的行、命中换算的行、下发的行都从 `plain_lines` 取，必然一致。
    pub fn to_styled(&self) -> StyledText {
        let lines: Vec<StyledText> = self.plain_lines().into_iter().map(|(t, _)| t).collect();
        StyledText::join(&lines, "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(title: Option<&str>, inline: bool, lines: &[&str]) -> TooltipSection {
        TooltipSection {
            title: title.map(StyledText::from),
            inline,
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, t)| TooltipLine {
                    text: (*t).into(),
                    raw: i as u16,
                })
                .collect(),
        }
    }

    #[test]
    fn plain_text_formats_titles_inline_and_untitled_sections() {
        let doc = TooltipDoc {
            sections: vec![
                sec(Some("编码"), false, &["vbg"]),
                sec(Some("Unicode"), true, &["U+597D"]),
                sec(Some("拼音"), false, &["好：hǎo", "人：rén"]),
                sec(Some("原文"), true, &["折成", "两行"]),
                sec(None, false, &["无标题"]),
            ],
        };
        assert_eq!(
            doc.to_plain_text(),
            "[编码]\nvbg\nUnicode: U+597D\n[拼音]\n好：hǎo\n人：rén\n原文: 折成\n两行\n无标题",
            "inline 段标题接第一条显示行，其余显示行照常另起"
        );
        assert_eq!(TooltipDoc::default().to_plain_text(), "");
    }

    /// 命中换算与纯文本同一份排列：标题行 → raw None；inline 首行 → 第一条原始行；
    /// 折行产生的多条显示行 → 同一条原始行。
    #[test]
    fn hit_at_line_follows_plain_text_layout() {
        let mut wrapped = sec(Some("原文"), false, &["折成", "两行"]);
        wrapped.lines[1].raw = 0;
        let doc = TooltipDoc {
            sections: vec![
                sec(Some("编码"), false, &["vbg"]),
                sec(Some("Unicode"), true, &["U+597D"]),
                wrapped,
            ],
        };
        let h = |section, raw_line| Some(TooltipHit { section, raw_line });
        assert_eq!(doc.hit_at_line(0), h(0, None));
        assert_eq!(doc.hit_at_line(1), h(0, Some(0)));
        assert_eq!(doc.hit_at_line(2), h(1, Some(0)));
        assert_eq!(doc.hit_at_line(3), h(2, None));
        assert_eq!(doc.hit_at_line(4), h(2, Some(0)));
        assert_eq!(
            doc.hit_at_line(5),
            h(2, Some(0)),
            "折行的第二条显示行指回同一原始行"
        );
        assert_eq!(doc.hit_at_line(6), None);
        assert_eq!(doc.to_plain_text().lines().count(), 6);
    }

    #[test]
    fn fingerprint_tracks_content() {
        let a = TooltipDoc {
            sections: vec![sec(Some("拼音"), false, &["你：nǐ"])],
        };
        let mut b = a.clone();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.sections.insert(0, sec(Some("编码"), false, &["wqvb"]));
        assert_ne!(a.fingerprint(), b.fingerprint(), "前面插入一段即不同");
    }

    /// 只改颜色（片段样式）时指纹不变、`PartialEq` 却不等；改 raw 下标则指纹变。
    #[test]
    fn fingerprint_ignores_styles_but_not_raw() {
        let a = TooltipDoc {
            sections: vec![sec(Some("拼音"), false, &["你：nǐ"])],
        };
        let mut styled = a.clone();
        let mut t = StyledText::new();
        t.push(
            "你",
            &SpanStyle {
                role: Some("char"),
                ..Default::default()
            },
        );
        t.push("：nǐ", &SpanStyle::default());
        styled.sections[0].lines[0].text = t;
        assert_eq!(a.fingerprint(), styled.fingerprint(), "只改颜色，指纹不变");
        assert_ne!(a, styled, "含样式的判等要能看出不同");
        let mut raw = a.clone();
        raw.sections[0].lines[0].raw = 3;
        assert_ne!(a.fingerprint(), raw.fingerprint());
    }

    /// 段名装饰 `[` `]`、inline 的 `: ` 带 title 角色；样式整块与纯文本同一排列。
    #[test]
    fn styled_decorations_are_title() {
        let doc = TooltipDoc {
            sections: vec![
                sec(Some("编码"), false, &["vbg"]),
                sec(Some("U"), true, &["x"]),
            ],
        };
        let s = doc.to_styled();
        assert_eq!(s.as_str(), doc.to_plain_text());
        let roles: Vec<(u32, u32, Option<&str>)> = s
            .spans()
            .iter()
            .map(|sp| (sp.start, sp.end, sp.role))
            .collect();
        // "[编码]\nvbg\nU: x"：[ 在 0..1，] 在 7..8，": " 在 14..16。
        assert_eq!(
            roles,
            vec![
                (0, 1, Some("title")),
                (7, 8, Some("title")),
                (14, 16, Some("title"))
            ]
        );
    }
}
