//! 「这是不是 emoji」——按 Unicode UTS #51 的 `Emoji` 属性判，不按 Unicode 块判。
//!
//! # ★★★ 为什么不能用块判
//!
//! [`crate::charblock`] 那张块表曾经兼职做过这件事：预设组 `"emoji"` 展开成五个块
//! （表情符号 / 杂项符号 / 装饰符号 / 杂项技术符号 / 区域指示符）。它在**两个方向上
//! 同时不准**，而这不是漏列几块能补上的：
//!
//! - **漏**：emoji 散落在约二十个块里。实测一份五笔 emoji 码表（4132 条），五块口径
//!   盖不到 182 条——`⬅ ⬛ ⭐ ⭕ 🀄 🃏 🅰 🆚 🈚 🉐 ⤴ ⤵` 整块不在表里，
//!   `▶ ◀ ↔ ↩ ‼ ™ ℹ ㊗ Ⓜ 〰 © ®` 所在的块则大部分是非 emoji、整块搬不动。
//! - **多**：「杂项符号」块里的 `♠ ☯ ☰`、「杂项技术符号」块里的 `⌘ ⌥` 一并被算作 emoji。
//!
//! 块是**显示域**的划分（回答「这个字符叫什么类」），emoji 是**字符属性**。两者正交，
//! 用其中一个近似另一个必然两头不准。属性表压成区间后只有一百多段、一次二分，
//! 判定成本与块判据同量级——当初用块不是性能取舍，只是手边没有这张表。
//!
//! # 取 `Emoji` 而不是 `Emoji_Presentation`
//!
//! 上游给了两条线，差集是「默认文本表现、加 `U+FE0F` 才变彩色」的两栖字符：
//!
//! | 属性 | 码位数 | 含义 | `©` | `▶` | `❤` | `🕵` | `😀` |
//! |---|---|---|---|---|---|---|---|
//! | `Emoji` | 1438 | **可以**当 emoji 渲染 | ✅ | ✅ | ✅ | ✅ | ✅ |
//! | `Emoji_Presentation` | 1219 | **默认**就是彩色 | ❌ | ❌ | ❌ | ❌ | ✅ |
//!
//! 取宽档，因为**词库存的是裸码位**：实测那份码表 1404 个字素簇里只有 10 个带 `FE0F`，
//! `❤ ☀ ✈ ♠ 🕵 🖥` 全是裸的。按 `Emoji_Presentation` 判会把 201 个（14%）真 emoji
//! 判成非 emoji——正是 [`crate::charblock`] 模块头那张表里标着**不安全**的方向。
//!
//! ⚠️ 代价是 `© ® ™ ↔ ▶ Ⓜ ▪` 这些「看着像符号」的也算 emoji。这是 Unicode 自己的
//! 立场（它们在 RGI 全表里都是 `fully-qualified` 条目，分在 `Symbols` 组），不是本仓
//! 的取舍。真要把它们摘出去，判据是 `emoji-test.txt` 的分组而不是这里的属性表——
//! 那是另一份数据、另一个量级，且会连 `✅ ❌ ⭕ 💯` 一起摘走。

use crate::charemoji_data::{
    EMOJI_BMP_LAST, EMOJI_FIRST, EMOJI_LAST, EMOJI_RANGES, EMOJI_SUPP_FIRST,
};

pub use crate::charemoji_data::UNICODE_EMOJI_VERSION;

/// 「emoji」这一类在配置与界面上的名字，**全仓唯一的字面量出处**。
///
/// 三个地方要用同一个名字：配置里的 `exclude_blocks = ["emoji"]`（[`crate::BlockMask`]
/// 解析）、常用字列表的类型列（[`crate::CharClass::name`]）、以及界面上那个勾选项。
/// 各写一份的话，改了其中一处的显示名，另外两处会静默失配——表现为「配置里那一行忽然
/// 不认了」或「类型列显示的名字勾不出来」，而两者都没有任何报错。
pub const EMOJI_CLASS_NAME: &str = "emoji";

/// 组合用括号键帽 `U+20E3`：`1` + 它（中间可有 `FE0F`）才构成 `1️⃣`。
const COMBINING_KEYCAP: char = '\u{20E3}';

/// 键帽序列的基字符——`# * 0-9`，共 12 个。
///
/// ★★★ 它们的 `Emoji` 属性为真，但**单独出现时不是 emoji**，`1` 就是数字一。
/// 这 12 个码位是 `Emoji=Yes` 里唯一一批需要上下文才能定性的：UTS #51 的 ED-13 把
/// 键帽定义成序列 `[0-9#*] FE0F? 20E3`，属性表只标了它的**基字符**。
///
/// 不排除掉的后果全都是静默的，且每一条都比「漏一个 emoji」严重：
/// - 免词频：所有数字候选不再学词频，表现为「数字选多少次都不往前排」；
/// - 生僻字准入：勾了 emoji 之后 `0`–`9` 会挤进生僻字候选；
/// - 常用字列表整类批量：一次「把 emoji 全设为生僻」把十个数字一起判掉。
///
/// `emoji_property_below_ascii_is_exactly_keycap_bases` 钉住这个集合与上游一致——
/// 上游哪天给 ASCII 区加了新 emoji，那条测试会红，而不是让这里悄悄漏判。
fn is_keycap_base(ch: char) -> bool {
    matches!(ch, '#' | '*' | '0'..='9')
}

/// 这个码位的 Unicode `Emoji` 属性为真吗——**原样反映属性表，不掺任何取舍**。
///
/// 需要「单独出现算不算 emoji」的场合用 [`is_emoji_standalone`]，需要判一段文本的用
/// [`text_has_emoji`]。三者分开是因为它们的答案对 `'1'` 不同，而把三种问题合并成一个
/// 函数正是上一版把「块」当「emoji」用时犯的错。
///
/// # 早退
///
/// 先用四个由生成器算出的边界排除绝大多数字符：汉字、假名、拉丁字母、PUA、全角形式
/// 全都落在表外或 BMP 与增补面之间那个十一万码位的大空洞里，三次比较即可返回。
/// 剩下的才进二分（一百多段，约七次比较）。
pub fn is_emoji(ch: char) -> bool {
    let c = ch as u32;
    // 两个早退条件：整体范围之外，或落在 BMP 末尾与增补面首段之间那个大空洞里。
    let in_span = (EMOJI_FIRST..=EMOJI_LAST).contains(&c);
    let in_gap = (EMOJI_BMP_LAST + 1..EMOJI_SUPP_FIRST).contains(&c);
    if !in_span || in_gap {
        return false;
    }
    // partition_point 给出首个 `start > c` 的位置，故候选区间是它的前一项。
    // 与 `charblock::block_index_of` 同形，正确性同样依赖表有序（`ranges_are_sorted`）。
    let i = EMOJI_RANGES.partition_point(|r| r.0 <= c);
    i.checked_sub(1).is_some_and(|i| c <= EMOJI_RANGES[i].1)
}

/// 这个字符**单独出现**时算 emoji 吗——即 [`is_emoji`] 去掉键帽基字符。
///
/// 这是逐字符扫描类消费者（词库扫描、单字符判定）该用的那个。
pub fn is_emoji_standalone(ch: char) -> bool {
    is_emoji(ch) && !is_keycap_base(ch)
}

/// 这段文本里**含有** emoji 吗（存在性语义）。
///
/// 与逐字符判的差别只在键帽：`1` 不是 emoji，`1️⃣`（`1` + `FE0F` + `20E3`）是。故先扫一遍
/// 收集「见过键帽基字符」与「见过 `U+20E3`」两个标志，两者同时为真才认这一串键帽。
///
/// ⚠️ 判据刻意**不做字素簇分割**：本函数在按键热路径上（每次按键 × 每个候选），而
/// 存在性语义下逐 `char` 与逐簇的差别只有键帽这一处，已在这里显式补上。若将来有消费者
/// 要问「整串**都**是 emoji 吗」（全称语义），不能顺手复用本函数——存在性下"更宽"是
/// 安全方向，全称语义下会反过来变成多判为真。
pub fn text_has_emoji(text: &str) -> bool {
    let (mut saw_keycap, mut saw_base) = (false, false);
    for ch in text.chars() {
        if ch == COMBINING_KEYCAP {
            saw_keycap = true;
        } else if is_keycap_base(ch) {
            saw_base = true;
        } else if is_emoji(ch) {
            return true;
        }
    }
    saw_keycap && saw_base
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表必须升序、互不重叠**且互不相邻**（相邻的两段本该合并）。
    ///
    /// 前两条是 [`is_emoji`] 二分的正确性前提；「互不相邻」是生成器的输出契约，
    /// 它一旦破了说明这张表被手改过——而手改一张生成物是本文件唯一不该发生的事。
    #[test]
    fn ranges_are_sorted() {
        for w in EMOJI_RANGES.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(a.0 <= a.1, "区间反了: {a:04X?}");
            assert!(
                a.1 + 1 < b.0,
                "{:04X?} 与 {:04X?} 重叠、失序或应当合并",
                a,
                b
            );
        }
    }

    /// 早退的四个边界必须与表本身一致，否则会静默漏判整片字符。
    #[test]
    fn early_out_bounds_agree_with_the_table() {
        assert_eq!(EMOJI_FIRST, EMOJI_RANGES[0].0);
        assert_eq!(EMOJI_LAST, EMOJI_RANGES[EMOJI_RANGES.len() - 1].1);
        assert_eq!(
            EMOJI_BMP_LAST,
            EMOJI_RANGES.iter().rfind(|r| r.1 <= 0xFFFF).unwrap().1
        );
        assert_eq!(
            EMOJI_SUPP_FIRST,
            EMOJI_RANGES.iter().find(|r| r.0 > 0xFFFF).unwrap().0
        );
        // 空洞里不能有 emoji——这正是早退敢整片跳过的依据。
        const { assert!(EMOJI_BMP_LAST < EMOJI_SUPP_FIRST) };
    }

    /// 二分与线性扫描必须处处一致。同 `charblock::binary_search_matches_linear_scan`：
    /// 早退 + 二分是两层加速，任何一层写错都只在**某些**码位上错，抽样测不出来。
    #[test]
    fn binary_search_matches_linear_scan() {
        let linear = |c: u32| EMOJI_RANGES.iter().any(|r| r.0 <= c && c <= r.1);
        // 全码位空间太大，扫 emoji 实际出没的两段 + 一段汉字（验早退不误杀也不误放）。
        let spans = [0x0000..=0x3400u32, 0x4E00..=0x4F00, 0x1F000..=0x1FBFF];
        for span in spans {
            for c in span {
                let Some(ch) = char::from_u32(c) else {
                    continue;
                };
                assert_eq!(is_emoji(ch), linear(c), "U+{c:04X} 二分与线性不一致");
            }
        }
    }

    /// ★ 键帽基字符的集合必须与上游一致。
    ///
    /// [`is_keycap_base`] 是本文件唯一一处**手写**的码位集合，它成立的前提是
    /// 「`Emoji=Yes` 在 ASCII 区里恰好只有 `# * 0-9`」。上游若给 ASCII 区加了新 emoji，
    /// 这条会红——否则那个新字符会被 `is_emoji_standalone` 无声地判成 emoji。
    #[test]
    fn emoji_property_below_ascii_is_exactly_keycap_bases() {
        let from_table: Vec<char> = (0u32..0x80)
            .filter_map(char::from_u32)
            .filter(|c| is_emoji(*c))
            .collect();
        let hand_written: Vec<char> = (0u32..0x80)
            .filter_map(char::from_u32)
            .filter(|c| is_keycap_base(*c))
            .collect();
        assert_eq!(
            from_table, hand_written,
            "ASCII 区的 emoji 码位集合与手写的不一致"
        );
        assert_eq!(from_table.len(), 12);
    }

    /// 真实样本：上一版块判据漏掉的那几批，逐个钉住。
    ///
    /// 全部取自用户实际使用的五笔 emoji 码表，都是词库里真存在的裸码位形态。
    #[test]
    fn covers_the_blocks_the_old_criterion_missed() {
        for ch in [
            '🀄', // 麻将牌，整块不在旧块表里
            '🃏', // 扑克牌，同上
            '🅰',
            '🆚', // 带圈字母数字补充，旧表只补了它尾巴上的区域指示符
            '🈚', '🉐', // 带圈表意文字补充
            '⬅', '⬛', '⭐', '⭕', // 杂项符号和箭头
            '⤴', '⤵', // 补充箭头 B
            '▶', '◀', '▪', '◽', // 几何图形，整块搬不动
            '↔', '↩', // 箭头
            '‼', '⁉', // 通用标点
            '™', 'ℹ', // 字母式符号
            '©', '®', // 拉丁文补充
            '〰', '〽', // CJK 符号和标点
            '㊗', // 带圈 CJK 字母及月份
            'Ⓜ',  // 带圈字母数字
        ] {
            assert!(is_emoji(ch), "{ch} 应判为 emoji");
            assert!(is_emoji_standalone(ch), "{ch} 单独出现也应算 emoji");
        }
    }

    /// 旧块判据已经覆盖的那些，换判据后不能反而丢了。
    #[test]
    fn still_covers_what_the_old_criterion_had() {
        for s in [
            "😀",         // 表情符号
            "⚽",         // 杂项符号（裸）
            "⚽\u{FE0F}", // 同上带 VS16
            "✅",         // 装饰符号
            "⌚",         // 杂项技术符号
            "🇨🇳",         // 区域指示符对
            "👍🏻",         // 带肤色修饰符
            "👨‍👩‍👧",         // ZWJ 家庭
            "❤",          // 默认文本表现，取宽档才留得住
            "🕵",          // 同上
        ] {
            assert!(text_has_emoji(s), "{s} 应命中 emoji");
        }
    }

    /// ★★★ 裸数字不是 emoji，键帽序列是。
    ///
    /// 这一对是取 `Emoji` 宽档时唯一需要额外判据的地方，两个方向都要钉：
    /// 只钉前者会漏掉键帽，只钉后者会让数字全军覆没。
    #[test]
    fn bare_keycap_bases_are_not_emoji_but_sequences_are() {
        for ch in ['0', '5', '9', '#', '*'] {
            assert!(is_emoji(ch), "{ch} 的 Emoji 属性确实为真");
            assert!(!is_emoji_standalone(ch), "但 {ch} 单独出现不是 emoji");
            assert!(!text_has_emoji(&ch.to_string()), "单个 {ch} 不该命中");
        }
        // 完整形态与词库里常见的省略 VS16 形态都要认。
        assert!(text_has_emoji("1\u{FE0F}\u{20E3}"), "1️⃣ 完整形态");
        assert!(
            text_has_emoji("1\u{20E3}"),
            "1⃣ 省略 VS16 的形态，词库里就是这么存的"
        );
        assert!(text_has_emoji("#\u{20E3}"));
        // 光有键帽或光有基字符都不算。
        assert!(!text_has_emoji("\u{20E3}"));
        assert!(!text_has_emoji("123"));
    }

    /// 正文字符一个都不能被抓走——误伤汉字的症状是「这个字选多少次都不往前排」，
    /// 而且完全静默。
    #[test]
    fn does_not_catch_text() {
        for s in [
            "我",
            "你好",
            "abc",
            "ABC",
            "、",
            "，",
            "。",
            "１２３",
            "ㄅ",
            "あ",
            "カ",
            "⿰",
            "℃",
            "±",
            "→",
            "■",
            "①",
            "㈠",
            "─",
            "∞",
            "—",
            "￥",
            "龘",
            "\u{E000}",
        ] {
            assert!(!text_has_emoji(s), "{s} 不该被判成 emoji");
        }
    }

    /// 混排文本按存在性命中——候选文本可能是「笑😀」这种。
    #[test]
    fn mixed_text_hits_by_existence() {
        assert!(text_has_emoji("笑😀"));
        assert!(text_has_emoji("😀笑"));
        assert!(!text_has_emoji(""));
        assert!(!text_has_emoji("   "));
    }
}
