//! 字符的 Unicode 块归类——**仅供显示与批量操作**，不参与任何判定。
//!
//! # ⛔ 与 [`crate::common`] 里的 `is_han` 是两张表，永远不要合并
//!
//! 两张表都在列举 Unicode 区间，看起来该统一，但**漏一块的后果完全不同**：
//!
//! | | 漏一块的后果 |
//! |---|---|
//! | `is_han`（判定域） | 那批字**恒判常用**、任何检索范围档下都放行——过滤静默失效 |
//! | 本表（显示域） | 那批字的类型列显示「其它」——一个不好看的标签 |
//!
//! issue #83 正是前者：`is_han` 逐块列举到扩展 H 末尾 `0x323AF`，Unicode 17 的扩展 J 从
//! `0x323B0` 起，**差一个码位**落到域外，于是用户的常用字候选里冒出一批无字形的生僻字。
//! 修法是补充平面按**平面**整体兜底，不再逐块。
//!
//! 本表则**必须**逐块——它的产出就是块名，兜底成一个大范围就没有信息量了。所以这里保留
//! 逐块列举，代价是新版 Unicode 的新块会落进「其它」，而那是可以接受的退化。
//! **正因为可接受，才不能让这张表回头去承担判定职责。**

/// 一个 Unicode 块。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharBlock {
    /// 中文块名，直接显示给用户。
    pub name: &'static str,
    /// 块的起止码位（闭区间）。批量操作按它圈定范围。
    pub start: u32,
    pub end: u32,
}

impl CharBlock {
    /// `2FF0-2FFF` 这样的范围文本，供界面显示与批量操作传参。
    pub fn range_text(&self) -> String {
        format!("{:04X}-{:04X}", self.start, self.end)
    }
}

/// 落在本表之外的字符统一归到这里。`start > end` 是刻意的**空区间**：批量操作拿它去圈
/// 范围会得到零个字符，天然拦住「把一堆互不相干的字符当成一类批量处理」。
const OTHER: CharBlock = CharBlock {
    name: "其它",
    start: 1,
    end: 0,
};

/// 块表，**按 `start` 升序**（`blocks_are_sorted_and_disjoint` 钉着）。
///
/// ⚠️ 扩展 I（`2EBF0–2EE5F`）在码位上夹在扩展 F 与扩展 G 之间，不是按字母顺序排的；
/// 扩展 J（`323B0–3347F`）同理排在扩展 H 之后。照字母顺序写会破坏有序性。
const BLOCKS: &[CharBlock] = &[
    b("ASCII", 0x0020, 0x007F),
    b("拉丁文补充", 0x00A0, 0x024F),
    b("希腊字母", 0x0370, 0x03FF),
    b("西里尔字母", 0x0400, 0x04FF),
    b("通用标点", 0x2000, 0x206F),
    b("上标与下标", 0x2070, 0x209F),
    b("货币符号", 0x20A0, 0x20CF),
    b("字母式符号", 0x2100, 0x214F),
    b("数字形式", 0x2150, 0x218F),
    b("箭头", 0x2190, 0x21FF),
    b("数学运算符", 0x2200, 0x22FF),
    b("杂项技术符号", 0x2300, 0x23FF),
    b("带圈字母数字", 0x2460, 0x24FF),
    b("制表符", 0x2500, 0x257F),
    b("方块元素", 0x2580, 0x259F),
    b("几何图形", 0x25A0, 0x25FF),
    b("杂项符号", 0x2600, 0x26FF),
    b("装饰符号", 0x2700, 0x27BF),
    b("CJK 部首补充", 0x2E80, 0x2EFF),
    b("康熙部首", 0x2F00, 0x2FDF),
    b("表意文字描述符", 0x2FF0, 0x2FFF),
    b("CJK 符号和标点", 0x3000, 0x303F),
    b("平假名", 0x3040, 0x309F),
    b("片假名", 0x30A0, 0x30FF),
    b("注音符号", 0x3100, 0x312F),
    b("谚文兼容字母", 0x3130, 0x318F),
    b("汉文标注", 0x3190, 0x319F),
    b("注音符号扩展", 0x31A0, 0x31BF),
    b("CJK 笔画", 0x31C0, 0x31EF),
    b("片假名语音扩展", 0x31F0, 0x31FF),
    b("带圈 CJK 字母及月份", 0x3200, 0x32FF),
    b("CJK 兼容符号", 0x3300, 0x33FF),
    b("扩展 A", 0x3400, 0x4DBF),
    b("基本汉字", 0x4E00, 0x9FFF),
    b("私用区", 0xE000, 0xF8FF),
    b("兼容汉字", 0xF900, 0xFAFF),
    b("CJK 兼容形式", 0xFE30, 0xFE4F),
    b("半角及全角形式", 0xFF00, 0xFFEF),
    // ⚠️ 表情符号在**平面 1**（1F300…），码位上排在扩展 B（20000…）之前。按「重要性」
    // 把它挪到表尾会破坏有序性——线性扫描碰巧仍能命中，但有序性一旦失守，日后换二分就错。
    b("表情符号", 0x1F300, 0x1FAFF),
    b("扩展 B", 0x20000, 0x2A6DF),
    b("扩展 C", 0x2A700, 0x2B73F),
    b("扩展 D", 0x2B740, 0x2B81F),
    b("扩展 E", 0x2B820, 0x2CEAF),
    b("扩展 F", 0x2CEB0, 0x2EBEF),
    b("扩展 I", 0x2EBF0, 0x2EE5F),
    b("兼容汉字补充", 0x2F800, 0x2FA1F),
    b("扩展 G", 0x30000, 0x3134F),
    b("扩展 H", 0x31350, 0x323AF),
    b("扩展 J", 0x323B0, 0x3347F),
];

const fn b(name: &'static str, start: u32, end: u32) -> CharBlock {
    CharBlock { name, start, end }
}

/// 这个字符属于哪个块；表外一律 [`OTHER`]。
pub fn block_of(ch: char) -> CharBlock {
    let c = ch as u32;
    // 表短（约 50 项）且只在列表渲染与批量操作时调用，线性扫描足够；二分要先保证有序，
    // 而有序性已经由测试钉住，真需要提速再换不迟。
    for blk in BLOCKS {
        if blk.start <= c && c <= blk.end {
            return *blk;
        }
    }
    OTHER
}

/// 这个块能不能整类批量操作。
///
/// ⛔ **默认字表管得着的块一律不行**（汉字、部首、笔画、PUA，即 `is_common_scope` 为真）。
/// 列表里 8104 个默认字全是汉字，对着一行「我」弹出「将『基本汉字』全部设为生僻」，点一下
/// 就是七千多条覆盖——整张常用字表当场作废，而这只是一次误点。那些块本就有默认字表在逐字
/// 管着，要调也该逐字调。
///
/// 判据取块的**两端**而不是某个代表字符：块与 `is_common_scope` 的区间边界并非处处对齐
/// （如「CJK 符号和标点」整块在域外，而「康熙部首」整块在域内），两端同时在域外才算数。
pub fn block_allows_bulk_edit(blk: &CharBlock) -> bool {
    if blk.start > blk.end {
        return false; // OTHER：空区间，批量操作没有意义
    }
    let ends_outside = |c: u32| char::from_u32(c).is_some_and(|ch| !crate::is_common_scope(ch));
    ends_outside(blk.start) && ends_outside(blk.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表必须按 `start` 升序且互不重叠——线性扫描「首个命中即返回」的正确性全靠它。
    /// 重叠时，插入顺序会悄悄决定归类结果，改表的人不会察觉。
    #[test]
    fn blocks_are_sorted_and_disjoint() {
        for w in BLOCKS.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            assert!(a.start <= a.end, "{} 区间反了", a.name);
            assert!(
                a.end < b.start,
                "{} (…{:04X}) 与 {} ({:04X}…) 重叠或失序",
                a.name,
                a.end,
                b.name,
                b.start
            );
        }
    }

    /// issue #83 里用户亲自列出的那几类，块名必须认得出来。
    #[test]
    fn classifies_the_blocks_from_issue_83() {
        for (ch, name) in [
            ('⿰', "表意文字描述符"),
            ('⿿', "表意文字描述符"),
            ('ㄅ', "注音符号"),
            ('ㆠ', "注音符号扩展"),
            ('あ', "平假名"),
            ('、', "CJK 符号和标点"),
            ('㈱', "带圈 CJK 字母及月份"),
            // ⚠️ ℃ 是 U+2103「字母式符号」，不是 CJK 兼容符号——后者是 ㎡(U+33A1) 那一类。
            // 两者在候选里长得像「同一种东西」，块归属却隔着一万多个码位。
            ('℃', "字母式符号"),
            ('㎡', "CJK 兼容符号"),
            ('😀', "表情符号"),
            // ⚠️ 部首块里的三点水是 ⺡(U+2EA1)；日常打出来的 氵(U+6C35) 是**基本汉字**。
            // 两个字形一模一样，块归属不同——按字形猜块必错。
            ('⺡', "CJK 部首补充"),
            ('氵', "基本汉字"),
            ('⼀', "康熙部首"),
            ('㇀', "CJK 笔画"),
            ('我', "基本汉字"),
            ('\u{3400}', "扩展 A"),
            ('\u{20000}', "扩展 B"),
            ('\u{2EBF0}', "扩展 I"),
            ('\u{323B0}', "扩展 J"),
            ('\u{E831}', "私用区"),
        ] {
            assert_eq!(block_of(ch).name, name, "{ch} 归类错了");
        }
        assert_eq!(block_of('\u{0}').name, "其它");
    }

    /// 批量操作的闸门：域外块开放，默认字表管得着的块一律关闭。
    ///
    /// 「基本汉字」这条是本测试的重点——放行它等于给出一个一键作废 7831 条默认判定的按钮。
    #[test]
    fn bulk_edit_only_for_blocks_outside_the_default_table() {
        for ch in ['ㄅ', 'ㆠ', '⿰', 'あ', '、', '㈱', '℃'] {
            assert!(
                block_allows_bulk_edit(&block_of(ch)),
                "{ch} 所在块应开放批量"
            );
        }
        for ch in [
            '我',
            '\u{3400}',
            '\u{20000}',
            '\u{323B0}',
            '氵',
            '⼀',
            '㇀',
            '\u{E831}',
        ] {
            assert!(
                !block_allows_bulk_edit(&block_of(ch)),
                "{ch} 所在块受默认字表管辖，必须禁止批量"
            );
        }
        // OTHER 是空区间，批量无意义。
        assert!(!block_allows_bulk_edit(&block_of('\u{0}')));
    }

    #[test]
    fn range_text_is_hex_padded() {
        assert_eq!(block_of('⿰').range_text(), "2FF0-2FFF");
        assert_eq!(block_of('\u{323B0}').range_text(), "323B0-3347F");
    }
}
