//! 字符的 Unicode 块归类——显示、批量操作，以及**退化方向安全**的成组判定。
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
//! 逐块列举，代价是新版 Unicode 的新块会落进「其它」。
//!
//! # ★ 本表什么时候可以承担判定职责
//!
//! 原先这里写的是「正因为那个退化可以接受，才不能让本表回头去承担判定职责」——一刀切的
//! 禁令。[`crate::charclass`] 出现后它被改写成一条**带判据的准入**，因为一刀切挡掉的是
//! 合法用法，而真正危险的是特定的**失败方向**：
//!
//! > **显示域的表可以承担判定职责，当且仅当「漏一块」的退化方向是安全的**
//! > ——即退回当前已有的行为，而不是产生一种新的失效。
//!
//! | 用途 | 漏一块的后果 | 方向 | 需要的缓解 |
//! |---|---|---|---|
//! | 类型列显示 | 标签显示「其它」 | 安全 | 无 |
//! | emoji 免词频 | 那批 emoji 照旧参与词频 | **安全**（= 改动前的行为） | 无 |
//! | 生僻字模式准入 | 那批字在该模式里**打不出** | **不安全** | 必须有「其它」兜底档 |
//! | `is_han`（对照） | 恒判常用、过滤静默失效 | 不安全 | 已改为按平面兜底 |
//!
//! ⇒ 新增消费者时先把自己填进这张表。填不出「安全」的那一栏，就不要用本表做判据。

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
/// ⚠️ 扩展 I（`2EBF0–2EE5F`）在码位上夹在扩展 F 与扩展 G 之间，不是按字母顺序排的；
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
    // ⚠️ 表情符号在**平面 1**（1F300…），码位上排在扩展 B（20000…）之前。按「重要性」
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
/// 写成常量而不是硬编码数字：块表中间插一块（本轮就插了「区域指示符」）会让所有后续
/// 下标平移，而早退返回的是下标，写错就是**整片汉字被归进邻近的块**且没有任何报错。
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
/// # 为什么从线性扫描换成二分
///
/// 原实现逐项扫描 ~50 项，注释里写着「只在列表渲染与批量操作时调用，线性扫描足够；
/// **真需要提速再换不迟**」。`charclass` 把本函数放上了按键热路径（每次按键 × 每个候选，
/// 五笔单字母下 78+ 个），那个「不迟」到了。
///
/// 两层加速，都不影响正确性：
/// 1. **基本汉字早退**——绝大多数候选是汉字，一次范围比较即可返回；该区与其余各块不相交。
/// 2. **二分**取代线性：`partition_point` 找到最后一个 `start <= c` 的块再验 `end`。
///
/// ⚠️ 二分的正确性**依赖块表有序且互不重叠**。`blocks_are_sorted_and_disjoint` 此前只是
/// 防「归类结果被插入顺序悄悄决定」，现在是**正确性前提**——那条测试的地位随本次改动升级，
/// 不要因为它看起来只是整洁性检查而放宽它。
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

    /// 表必须按 `start` 升序且互不重叠。
    ///
    /// ⚠️ **这条测试的地位升级过**：原先它防的是「重叠时插入顺序会悄悄决定归类结果」——
    /// 一种归类漂移。[`block_index_of`] 改用二分之后，有序性成了**查找算法的正确性前提**：
    /// 表一旦失序，`partition_point` 会直接返回错误的块，而不只是某个有争议的归类。
    /// 别因为它看起来像整洁性检查就放宽它。
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

    /// 早退用的下标必须真的指向「基本汉字」。
    ///
    /// 本轮往表中间插了「区域指示符」，所有后续下标随之平移——若早退还硬编码着旧数字，
    /// 表现是**整片汉字被归进邻近的块**，且没有任何报错。常量由编译期扫表算出正是为了
    /// 免疫这种平移，这条测试钉住「扫表的判据（`start == 0x4E00`）没有失配」。
    #[test]
    fn basic_han_index_points_at_the_right_block() {
        assert_eq!(BLOCKS[BASIC_HAN_IDX].name, "基本汉字");
        assert_eq!(block_of('我').name, "基本汉字");
        assert_eq!(block_of('\u{4E00}').name, "基本汉字");
        assert_eq!(block_of('\u{9FFF}').name, "基本汉字");
    }

    /// **二分 + 早退 必须与原来的线性扫描逐字符等价。**
    ///
    /// 这条是本次算法替换的回归锁：性能改写最怕的不是崩，而是**某几个码位悄悄换了归类**
    /// ——类型列上看不出来，拿它做准入判据时却是「这批字打不出」。故不抽样，直接遍历
    /// 每个块的两端与相邻块之间的空隙，把边界全覆盖掉。
    #[test]
    fn binary_search_matches_linear_scan() {
        fn linear(ch: char) -> CharBlock {
            let c = ch as u32;
            for blk in BLOCKS {
                if blk.start <= c && c <= blk.end {
                    return *blk;
                }
            }
            OTHER
        }

        let mut probes: Vec<u32> = vec![0, 0x1F, 0x10FFFF];
        for blk in BLOCKS {
            // 块内两端与中点，以及紧邻两侧的空隙码位。
            probes.extend([
                blk.start.saturating_sub(1),
                blk.start,
                blk.start + (blk.end - blk.start) / 2,
                blk.end,
                blk.end + 1,
            ]);
        }
        for c in probes {
            let Some(ch) = char::from_u32(c) else {
                continue; // 代理区等非法标量值，两条路径都够不着
            };
            assert_eq!(
                block_of(ch).name,
                linear(ch).name,
                "U+{c:04X} 的归类被算法替换改变了"
            );
        }
    }

    /// 国旗的两个区域指示符必须各自归到「区域指示符」块。
    ///
    /// 补这一块之前它们落在「其它」——emoji 预设组因此带不出国旗，而这在类型列上
    /// 只表现为一个不起眼的「其它」标签。
    #[test]
    fn regional_indicators_are_classified() {
        assert_eq!(block_of('\u{1F1E8}').name, "区域指示符"); // 🇨
        assert_eq!(block_of('\u{1F1F3}').name, "区域指示符"); // 🇳
        assert_eq!(block_of_cluster("🇨🇳").name, "区域指示符");
        // 邻接的「表情符号」块不受影响。
        assert_eq!(block_of('😀').name, "表情符号");
    }
}
