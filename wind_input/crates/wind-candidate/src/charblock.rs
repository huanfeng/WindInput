//! 字符的 Unicode 块归类——显示、批量操作，以及退化方向安全的成组判定。
//!
//! # 与 [`crate::common`] 里的 `is_han` 是两张表，不要合并
//!
//! 漏一块的后果不同：`is_han`（判定域）漏块会让那批字恒判常用、过滤静默失效，故按平面
//! 整体兜底；本表（显示域）漏块只是类型列显示「其它」，且产出就是块名，必须逐块列举。
//! 见 `docs/design/charset-classification.md` §1。
//!
//! # 本表什么时候可以承担判定职责
//!
//! 当且仅当「漏一块」的退化方向是安全的（退回既有行为，而不是产生新的失效）：
//!
//! | 用途 | 漏一块的后果 | 方向 | 需要的缓解 |
//! |---|---|---|---|
//! | 类型列显示 | 标签显示「其它」 | 安全 | 无 |
//! | emoji 免词频 | 那批 emoji 照旧参与词频 | 安全 | 无 |
//! | 生僻字模式准入 | 那批字在该模式里打不出 | 不安全 | 必须有「其它」兜底档 |
//! | `is_han`（对照） | 恒判常用、过滤静默失效 | 不安全 | 已改为按平面兜底 |
//!
//! 新增消费者时先把自己填进这张表，填不出「安全」就不要用本表做判据。

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
    ///
    /// [`OTHER`] 那种空区间返回**空串**：它的 `start > end` 是刻意的哨兵值，直接格式化
    /// 会得到 `0001-0000` 这种没有意义、还会漏进界面的字符串。
    pub fn range_text(&self) -> String {
        if self.start > self.end {
            return String::new();
        }
        format!("{:04X}-{:04X}", self.start, self.end)
    }
}

/// 落在本表之外的字符统一归到这里。`start > end` 是刻意的**空区间**：批量操作拿它去圈
/// 范围会得到零个字符，天然拦住「把一堆互不相干的字符当成一类批量处理」。
pub(crate) const OTHER: CharBlock = CharBlock {
    name: "其它",
    start: 1,
    end: 0,
};

/// 块表，**按 `start` 升序**（`blocks_are_sorted_and_disjoint` 钉着）。
///
/// 扩展 I（`2EBF0–2EE5F`）在码位上夹在扩展 F 与扩展 G 之间，不是按字母顺序排的；
/// 扩展 J（`323B0–3347F`）同理排在扩展 H 之后。照字母顺序写会破坏有序性。
pub(crate) const BLOCKS: &[CharBlock] = &[
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
    // 国旗 🇨🇳 = 两个**区域指示符**（`U+1F1E8 U+1F1F3`），不在「表情符号」块内——它起于
    // `1F300`，而区域指示符在 `1F1E6`。补这一块之前 `block_of('🇨')` 返回「其它」，于是
    // emoji 预设组勾了也带不出国旗。
    b("区域指示符", 0x1F1E6, 0x1F1FF),
    // 表情符号在**平面 1**（1F300…），码位上排在扩展 B（20000…）之前。按「重要性」
    // 把它挪到表尾会破坏有序性——[`block_index_of`] 是二分，有序性一失守就直接错。
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

/// 「基本汉字」在 [`BLOCKS`] 里的下标，编译期算出——[`block_index_of`] 的早退要用它。
///
/// 不硬编码数字：块表中间插一块会让后续下标平移，写错就是整片汉字静默归进邻近的块。
/// `basic_han_index_points_at_the_right_block` 钉着它。
const BASIC_HAN_IDX: usize = {
    let mut i = 0;
    while i < BLOCKS.len() {
        if BLOCKS[i].start == 0x4E00 {
            break;
        }
        i += 1;
    }
    i
};

/// 扫不到「基本汉字」时 [`BASIC_HAN_IDX`] 会等于 `BLOCKS.len()`，而早退直接把它当下标
/// 返回 ⇒ [`block_of`] 越界 panic。让这种情况在编译期就失败，而不是等某次按键时崩。
const _: () = assert!(
    BASIC_HAN_IDX < BLOCKS.len(),
    "块表里找不到起于 U+4E00 的「基本汉字」块"
);

/// 这个字符落在 [`BLOCKS`] 的哪一项；表外为 `None`。
///
/// `charclass::BlockMask`（已降为测试对照）按下标建位集，故这里给的是下标而不是
/// [`CharBlock`]；[`crate::CharsetRegistry`] 的区间分段表同样按下标。
///
/// 在按键热路径上（每键 × 每候选）：先做基本汉字早退，再 `partition_point` 二分。
/// 二分依赖块表有序且互不重叠，`blocks_are_sorted_and_disjoint` 是正确性前提，不要放宽。
pub(crate) fn block_index_of(ch: char) -> Option<usize> {
    let c = ch as u32;
    if (0x4E00..=0x9FFF).contains(&c) {
        return Some(BASIC_HAN_IDX);
    }
    // partition_point 给出首个 `start > c` 的位置，故候选块是它的前一项。
    let i = BLOCKS.partition_point(|blk| blk.start <= c);
    let idx = i.checked_sub(1)?;
    (c <= BLOCKS[idx].end).then_some(idx)
}

/// 区块表：`(名, 起, 止)`，按 `start` 升序且互不重叠。
///
/// 只给 `gen_block_charsets` 生成 `data/charsets/blocks.yaml` 用。判定不要走这里，那归
/// [`crate::CharsetRegistry`]；两者不漂移由
/// `factory_blocks_match_the_block_table_codepoint_by_codepoint` 钉住。
pub fn block_table() -> &'static [(&'static str, u32, u32)] {
    // BLOCKS 是 `CharBlock` 结构体数组，这里投影成元组：让生成器不必依赖那个类型，
    // 也就不会有人顺手从生成器那边给它加字段。
    static TABLE: std::sync::OnceLock<Vec<(&'static str, u32, u32)>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| BLOCKS.iter().map(|b| (b.name, b.start, b.end)).collect())
}

/// 「其它」兜底档的名字（区块表之外的一切）。
pub fn other_block_name() -> &'static str {
    OTHER.name
}

/// 预设组「符号」的成员块名 —— 标点、数学、图形这一类**非 emoji** 的符号块。
///
/// 与 emoji 刻意不相交，否则两个开关的关系说不清。emoji 与非 emoji 混住的三块（杂项符号、
/// 装饰符号、杂项技术符号）不整块列在这里，由 [`PRESET_SYMBOL_MIXED_BLOCKS`] 扣掉 emoji
/// 字表后并入。本表其余块整块并入，仍混有少量 emoji（`‼ ™ ↔ ▶` 等），这是既有口径。
///
/// 只收 `is_han ∪ is_pua` 域外的块：域内的本就是生僻字模式的默认输出，列进来勾了也没变化。
/// 块名对不上会让那一块静默消失，`gen_block_charsets` 在生成时核对数量。
const PRESET_SYMBOL_BLOCKS: &[&str] = &[
    "通用标点",
    "上标与下标",
    "货币符号",
    "字母式符号",
    "数字形式",
    "箭头",
    "数学运算符",
    "带圈字母数字",
    "制表符",
    "方块元素",
    "几何图形",
    "表意文字描述符",
    "CJK 符号和标点",
    "带圈 CJK 字母及月份",
    "CJK 兼容符号",
    "CJK 兼容形式",
    "半角及全角形式",
];

/// 预设组「符号」的成员块名。只给 `gen_block_charsets` 用（同 [`block_table`]）。
pub fn preset_symbol_block_names() -> &'static [&'static str] {
    PRESET_SYMBOL_BLOCKS
}

/// 预设组「符号」里**扣掉 emoji 后**才并入的块：emoji 与非 emoji 混住的三块。
///
/// 扣减用的是出厂 `charsets/emoji.yaml` 那份精确字表，由 `gen_block_charsets` 在生成时做。
const PRESET_SYMBOL_MIXED_BLOCKS: &[&str] = &["杂项技术符号", "杂项符号", "装饰符号"];

/// 预设组「符号」里扣掉 emoji 后并入的块名。只给 `gen_block_charsets` 用（同 [`block_table`]）。
pub fn preset_symbol_mixed_block_names() -> &'static [&'static str] {
    PRESET_SYMBOL_MIXED_BLOCKS
}

/// 这个字符属于哪个块；表外一律 [`OTHER`]。
pub fn block_of(ch: char) -> CharBlock {
    block_index_of(ch).map_or(OTHER, |i| BLOCKS[i])
}

/// 一个**字素簇**属于哪个块——按**首个** `char` 判。
///
/// `👨‍👩‍👧` 的首码位是 `U+1F468`（表情符号），后面跟的 ZWJ 与其余成员不改变它是什么类；
/// `1️⃣` 的首码位是数字 `1`，归「ASCII」也说得过去（它本就是被 keycap 化的数字）。
/// 空串归 [`OTHER`]。
pub fn block_of_cluster(cluster: &str) -> CharBlock {
    cluster.chars().next().map(block_of).unwrap_or(OTHER)
}

/// 这个块能不能整类批量操作。
///
/// 默认字表管得着的块（`is_common_scope` 为真）一律不行：一次误点「将『基本汉字』全部设为
/// 生僻」就是七千多条覆盖，整张常用字表作废。那些块应逐字调。
///
/// 判据取块的两端：块与 `is_common_scope` 的边界并非处处对齐，两端同时在域外才算数。
pub fn block_allows_bulk_edit(blk: &CharBlock) -> bool {
    if blk.start > blk.end {
        return false; // OTHER：空区间，批量操作没有意义
    }
    let ends_outside = |c: u32| char::from_u32(c).is_some_and(|ch| !crate::is_common_scope(ch));
    ends_outside(blk.start) && ends_outside(blk.end)
}

#[cfg(test)]
#[path = "charblock_tests.rs"]
mod tests;
