//! **成组的**字符区块判定：把若干 Unicode 块打成一个集合，供配置驱动的准入/排除使用。
//!
//! [`crate::charblock`] 回答「这一个字符属于哪个块」（单个，用于显示与批量操作）；
//! 本模块回答「这个候选**属不属于这一组块**」（成组，用于判定），并保证判定足够便宜到
//! 可以放上按键热路径。
//!
//! 两个消费者：
//!
//! | 消费者 | 用途 | 漏一块的方向 |
//! |---|---|---|
//! | emoji 免词频 | 正常输入里 emoji 不参与词频学习与重排 | 安全（照旧参与） |
//! | 生僻字模式准入 | 哪些区块的字符算「要在该模式里看到的」 | 不安全，须配「其它」兜底档 |
//!
//! 准入判据见 [`crate::charblock`] 模块头那张表——**新增消费者先把自己填进去**。
//!
//! # ★ 组名 `"emoji"` 不是一组块，走的是字符属性
//!
//! 它曾经展开成五个块名，而 emoji 散落在约二十个块里、其中多数块又大部分是非 emoji
//! ——两个方向同时不准，且补块补不齐（论证见 [`crate::charemoji`] 模块头）。现在它
//! 占一个专用位，判定转交 [`crate::is_emoji_standalone`]。
//!
//! ⇒ 配置写法与出厂值**一个字都没变**（`exclude_blocks = ["emoji"]` 照旧），变的只是
//! 那个名字底下问的是什么。块名（`"表情符号"`）仍然可以单独配，仍然按块判。
//!
//! # 为什么是位集而不是 `Vec<CharBlock>`
//!
//! 判定发生在每次按键 × 每个候选上（五笔单字母下 78+ 个候选）。位集把「属于这一组吗」
//! 压成一次移位加一次与运算，且 [`BlockMask`] 是 `Copy` 的，取用时不必克隆、不必借用
//! 配置。配置解析只在装载期做一次。

use crate::charblock::{BLOCKS, OTHER, block_index_of};

/// 一组 Unicode 块的位集，按 [`BLOCKS`] 的下标建位。
///
/// **默认为空集**，而空集的所有判定恒假——这正是「没配过这个功能的用户零成本」那条
/// 路径：调用方先问 [`is_empty`](Self::is_empty) 再决定要不要进循环。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockMask(u64);

/// 位宽守门：块表长到 64 项以上时，`1u64 << idx` 会静默丢掉高位的块——那些块从此
/// **永远无法被选中**，配置里写了也不报错。让它在编译期就失败。
///
/// 溢出时的两条出路：把 [`BlockMask`] 换成 `u128`（表示层改一处，判定成本几乎不变），
/// 或合并相邻的细碎块（会改变类型列的显示粒度，须一并确认界面）。
const _: () = assert!(
    BLOCKS.len() <= 62,
    "块表超出 BlockMask 的位宽：改用 u128，或合并相邻块（最高两位留给「其它」兜底档与 emoji 属性档）"
);

/// 「其它」兜底档占用的位——[`OTHER`] 不在 [`BLOCKS`] 里（它是块表**之外**的一切），
/// 没有下标可用，故单独占最高位。位宽断言因此留出这一位（与 [`EMOJI_BIT`] 各占一位）。
///
/// ★ 这一档存在的理由：块表逐块列举一份仍在增长的 Unicode 区间，新版本的新块必然落进
/// 「其它」。对**准入**类消费者（生僻字模式）而言，漏一块 = 那批字打不出——不安全的
/// 方向。给出这一档，新块就落进一个用户控制得到的开关，而不是静默消失。
const OTHER_BIT: u64 = 1 << 63;

/// emoji 属性档占用的位。置位后判定转交 [`crate::is_emoji_standalone`]，**与块位集无关**。
///
/// # ★★★ 为什么它不再是一组块
///
/// 上一版把 `"emoji"` 展开成五个块名（表情符号 / 杂项符号 / 装饰符号 / 杂项技术符号 /
/// 区域指示符）。实测一份五笔 emoji 码表（4132 条），那个口径**两个方向同时不准**：
///
/// - **漏 182 条**：`⬅ ⬛ ⭐ ⭕ 🀄 🃏 🅰 🆚 🈚 🉐 ⤴ ⤵` 所在的块根本不在块表里；
///   `▶ ◀ ↔ ↩ ‼ ™ ℹ ㊗ Ⓜ 〰 © ®` 所在的块大部分是非 emoji，整块搬进来会连
///   `← → ▲ ◆` 一起搬；
/// - **多收**：「杂项符号」块内的 `♠ ☯ ☰`、「杂项技术符号」块内的 `⌘ ⌥` 一并算 emoji。
///
/// 补块补不齐，因为**块是显示域的划分、emoji 是字符属性，两者正交**。换成属性表后
/// 判定成本仍是一次二分（151 段），与块判据同量级——当初用块不是性能取舍。
///
/// ⚠️ 换判据后 emoji 属性与「符号」预设组**刻意相交**（`▶ ↔ ▪ 〰` 两边都命中）。这不再
/// 是设计缺陷：`presets_are_disjoint` 当年要求不相交，是因为两个组同在「块」这一个轴上，
/// 重叠会让「勾了符号，emoji 那个还有什么用」说不清楚。现在两者不同轴，重叠是必然且无害的
/// ——同一个字符既属于「箭头」这个块，又具有 emoji 属性。
const EMOJI_BIT: u64 = 1 << 62;

/// emoji 属性档在配置里的名字。取自 [`crate::EMOJI_CLASS_NAME`]，**不另写一份字面量**
/// ——常用字列表的类型列显示的是同一个名字，两处各写一份就会静默失配。
///
/// ⚠️ 它与块名同处一个命名空间（[`BlockMask::from_config`] 先认它、再查组、最后查块），
/// 故不得与任何块名相同——由 `group_names_do_not_collide_with_block_names` 钉住。
const EMOJI_GROUP: &str = crate::EMOJI_CLASS_NAME;

/// 预设组「符号」：标点、数学、图形这一类**非 emoji** 的符号块。
///
/// ⚠️ 本组与 [`EMOJI_BIT`] 那一档**相交**，且这是对的：`▶ ↔ ▪ 〰 ‼` 既落在本组的块里，
/// 又具有 emoji 属性。两者不同轴——本组问「这个字符属于哪个 Unicode 块」，emoji 档问
/// 「这个字符的 `Emoji` 属性为不为真」。早先要求两组不相交，前提是它们同在块这一个轴上。
///
/// ★ 仍**不含**「杂项符号」「装饰符号」「杂项技术符号」（虽然名字里也带「符号」）：那三块
/// 里绝大多数是 `⚽ ✅ ☀ ♠` 这类图画符号，划进「符号」组会让勾了它的人意外地把 emoji
/// 一起关掉。`symbols_preset_excludes_the_pictographic_blocks` 钉住这个归属。
///
/// ★ 只收 `is_han ∪ is_pua` **域外**的块。部首、康熙部首、CJK 笔画、各扩展区都在
/// `is_han` 里（见 `common::is_han`），本来就是生僻字模式的默认输出，列进来是多余的
/// 开关——用户勾了没变化，只会以为坏了。
const PRESET_SYMBOLS: &[&str] = &[
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

/// 预设组名 → 成员块名。配置里这两种名字都收。
///
/// ⚠️ 组名与块名同处一个命名空间（`from_config` 先认 [`EMOJI_GROUP`]、再查组、最后查块），
/// 故**组名不得与任何块名相同**——否则同一个名字有两种解释，而先查组的写法会让块名那一侧
/// 静默失效。由 `group_names_do_not_collide_with_block_names` 钉住。
///
/// ⚠️ `"emoji"` **不在这张表里**：它不展开成块名，而是置 [`EMOJI_BIT`] 走字符属性。
const PRESETS: &[(&str, &[&str])] = &[("符号", PRESET_SYMBOLS)];

impl BlockMask {
    /// 空集：所有判定恒假。
    pub const EMPTY: Self = Self(0);

    /// 空集判定——调用方据此**整条绕开**本功能。
    ///
    /// 绝大多数用户两个消费者都不开，此时判定的成本应当是零而不是「很小」。同款闸门见
    /// `CommonChars::has_multi_char_keys`（那里实测：闸门关着 = 改动前的成本）。
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// 从配置解析。接受**块名**（`"表情符号"`）与**预设组名**（`"emoji"`）两种写法。
    ///
    /// 返回 `(掩码, 未识别的名字)`。**不在这里报警告**：本 crate 刻意只依赖 serde 与
    /// unicode-segmentation，没有 `tracing`；更重要的是「怎么报」属于调用方的事——
    /// 配置层要 warn 到日志，设置页要标红那一行，判定层不该替它们决定。
    ///
    /// 未识别项**跳过而不是整条失败**：配置是用户可手改的文本，一个拼错的块名不该让
    /// 整组规则失效（同 `FilterMode::from_config` 的未知值回退）。
    pub fn from_config<S: AsRef<str>>(names: &[S]) -> (Self, Vec<String>) {
        let mut mask = Self::EMPTY;
        let mut unknown = Vec::new();
        for raw in names {
            let name = raw.as_ref().trim();
            if name.is_empty() {
                continue;
            }
            // `"emoji"` 走字符属性而不是块位集，故认在最前面，也不进 PRESETS 那张表。
            if name == EMOJI_GROUP {
                mask.0 |= EMOJI_BIT;
                continue;
            }
            if let Some((_, members)) = PRESETS.iter().find(|(id, _)| *id == name) {
                for m in *members {
                    mask.insert_block_named(m);
                }
                continue;
            }
            if !mask.insert_block_named(name) {
                unknown.push(name.to_string());
            }
        }
        (mask, unknown)
    }

    /// 按块名置位；块名不存在返回 false（调用方据此收集未识别项）。
    fn insert_block_named(&mut self, name: &str) -> bool {
        // 「其它」不在块表里，单独占最高位（见 OTHER_BIT）。名字取自 `charblock::OTHER`
        // 而不是写死字面量——两处各写一份的话，块表那边改了显示名，这里就静默失配，
        // 表现为配置里那一行「拼错了」。
        if name == OTHER.name {
            self.0 |= OTHER_BIT;
            return true;
        }
        match BLOCKS.iter().position(|b| b.name == name) {
            Some(i) => {
                self.0 |= 1u64 << i;
                true
            }
            None => false,
        }
    }

    /// 这个字符落在本组内吗。表外字符（[`crate::charblock`] 的「其它」）只有显式配了
    /// 「其它」兜底档才算命中。
    ///
    /// ⚠️ emoji 档问的是 [`crate::is_emoji_standalone`] 而不是 `is_emoji`：`0`–`9`、`#`、`*`
    /// 的 `Emoji` 属性为真（它们是键帽 `1️⃣` 的基字符），但**单独出现时是数字不是 emoji**。
    /// 逐字符这一侧没有上下文可判，只能按「单独出现」定性；成串的键帽由
    /// [`contains_text`](Self::contains_text) 认。
    pub fn contains_char(&self, ch: char) -> bool {
        if self.0 & EMOJI_BIT != 0 && crate::is_emoji_standalone(ch) {
            return true;
        }
        match block_index_of(ch) {
            Some(i) => self.0 & (1u64 << i) != 0,
            // 块表之外的字符 ⇒ 归「其它」，只有显式配了这一档才算命中。
            None => self.0 & OTHER_BIT != 0,
        }
    }

    /// 这段文本**含有**本组内的字符吗（任一 `char` 命中即真）。
    ///
    /// # 为什么逐 `char` 而不是逐字素簇
    ///
    /// 两点，缺一都不足以定案：
    ///
    /// 1. **本函数在热路径上**，而字素簇分割明显更贵（`CommonChars` 为此专门加了
    ///    `has_multi_char_keys` 闸门去绕开它）。
    /// 2. **对「含 X 即算」这种存在性语义，逐 char 比逐簇更宽，而更宽的方向是安全的**：
    ///    簇的首码位是 emoji 时两者都命中；簇的**非首**码位是 emoji 时（`1️⃣` 的
    ///    `U+20E3`）只有逐 char 命中。多命中一个装饰性组合簇，后果是它也免了词频；
    ///    漏掉它，后果是用户觉得开关没生效。
    ///
    /// ⚠️ 这条推理**只对存在性语义成立**。若将来有消费者要问「整串**都**属于本组吗」
    /// （全称语义），逐 char 的宽松方向会反过来变成「多判为真」，那时必须重新论证，
    /// 不能顺手复用本函数。
    ///
    /// ⚠️ 早先这段注释举的例子是「`1️⃣` 的 `U+20E3` 只有逐 char 命中」——**那是错的**：
    /// `U+20E3` 在组合用记号补充块里，压根不在块表内，逐 char 也只会落到「其它」兜底档。
    /// 键帽真正被认出来是靠下面这句 [`crate::text_has_emoji`]，它显式判「基字符 + `U+20E3`
    /// 同时出现」。举错的例子比没有例子更糟：它让人以为这个洞已经被堵上了。
    pub fn contains_text(&self, text: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.0 & EMOJI_BIT != 0 && crate::text_has_emoji(text) {
            return true;
        }
        // emoji 档已经问过了，剩下的只按块查——否则循环里每个 char 都要再问一遍属性表，
        // 而上面那句为假就意味着串里没有任何 char 能在属性侧命中。
        let blocks_only = Self(self.0 & !EMOJI_BIT);
        if blocks_only.is_empty() {
            return false;
        }
        text.chars().any(|c| blocks_only.contains_char(c))
    }
}

/// 生僻字模式的候选准入：这条候选要不要出现在生僻字模式的列表里。
///
/// # ★ 判据不是 `!is_common`
///
/// [`crate::CommonChars::is_string_common`] 对**默认字表管辖域之外**的字符（emoji、注音、
/// 假名、标点）恒返回 `true`——那里的语义是「忽略、不拖累整串」，不是「它很常用」。
/// 直接取反会把这些字符**恒判为不生僻**从而全部滤掉，于是「生僻字模式里看不到 emoji」，
/// 而代码里找不到任何一处写着要滤掉它们。
///
/// 故准入写成**正向白名单**：
///
/// | 字符 | `is_string_common` | 进不进 |
/// |---|---|---|
/// | 生僻汉字（域内、不在默认表） | false | ✅ |
/// | 常用汉字 | true | ❌ |
/// | 用户手动设成生僻的**任何**字符 | false（覆盖优先） | ✅ |
/// | emoji / 注音 / 假名（未表态） | true（被忽略） | 只有 `extra` 圈中才进 |
///
/// `extra` 就是那道显式纳入：把域外区块按名字加进来（`input.rare_char.include_blocks`）。
///
/// # 只出单字
///
/// 用户 2026-08-24 拍板「严格只出单字」+「严格过滤，空了就空着」。判「一个字」走
/// [`crate::single_markable_char`]（UAX #29 字素簇），故 `⚽️`(2 码位)、`👨‍👩‍👧`(5 码位) 都算
/// 一个字——⛔ 别退回自己列举 Unicode 规则去数码位，那条路本仓已经走死过一次。
///
/// # 这个函数刻意不知道「模式是怎么进入的」
///
/// 入参只有候选文本与两份判定数据，没有 `State`、没有 `ModeKind`。当前入口是「顶字进入
/// 独立模式」，而将来若要加「保留编码、原地把候选换成生僻字」那种入口，本函数原样复用
/// ——过滤逻辑与进入方式是两件事，混在一起就会为了第二个入口把第一个也改一遍。
pub fn rare_admits(text: &str, cc: &crate::CommonChars, extra: BlockMask) -> bool {
    // 只出单字：多字词、空串、纯空白一律不进。
    let Some(unit) = crate::single_markable_char(text) else {
        return false;
    };
    !cc.is_string_common(unit) || extra.contains_text(unit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommonChars;

    fn emoji_mask() -> BlockMask {
        let (m, unknown) = BlockMask::from_config(&["emoji"]);
        assert!(unknown.is_empty(), "预设组名必须被认得");
        m
    }

    /// 同 `emoji_preset_covers_real_world_samples`：用样本钉住，不核对区间表。
    #[test]
    fn symbols_preset_covers_real_world_samples() {
        let (m, unknown) = BlockMask::from_config(&["符号"]);
        assert!(unknown.is_empty(), "预设组名必须被认得");
        for s in [
            "—", // 通用标点：破折号
            "∞", // 数学运算符。★ 别拿 ± 当样本：它是 U+00B1，在「拉丁文补充」块里
            //（与 é ñ ü 同块），而那一块是字母、不归本组——第一版就这么写错过
            "→",  // 箭头
            "①",  // 带圈字母数字
            "℃",  // 字母式符号
            "￥", // 货币符号
            "─",  // 制表符
            "■",  // 几何图形
            "、", // CJK 符号和标点
            "㈠", // 带圈 CJK 字母及月份
            "Ａ", // 半角及全角形式
            "⿰", // 表意文字描述符（拆字的间架结构）
        ] {
            assert!(m.contains_text(s), "「符号」组应含 {s:?}");
        }
    }

    /// ★ 「符号」组不含那三块图画符号块。
    ///
    /// 名字最容易骗人的三块（杂项符号 / 装饰符号 / 杂项技术符号）字面上都带「符号」，
    /// 内容却是 `⚽ ✅ ☀ ♠ ⌚` 这类图画。划进「符号」组会让勾了它的人意外地连 emoji
    /// 一起关掉。这条测试是那个归属的唯一书面凭据。
    ///
    /// ⚠️ 取代了原先的 `presets_are_disjoint`。那条要求「符号」与 emoji 两组**不相交**，
    /// 前提是两者同在「块」这一个轴上；emoji 换成字符属性判之后两者不同轴，`▶ ↔ ▪`
    /// 必然同时命中，再要求不相交就是要求判据回退。
    #[test]
    fn symbols_preset_excludes_the_pictographic_blocks() {
        let (_, symbols) = PRESETS
            .iter()
            .find(|(id, _)| *id == "符号")
            .expect("「符号」组必须在表里");
        for blk in ["杂项符号", "装饰符号", "杂项技术符号"] {
            assert!(
                !symbols.contains(&blk),
                "「符号」组不该含图画符号块 {blk}——勾它的人不会想连 emoji 一起关掉"
            );
        }
    }

    /// ★ 「符号」组与 emoji 属性档**相交，而且这是对的**。
    ///
    /// 单独钉一条，是因为「两组重叠」在上一版是要修的缺陷、这一版是刻意的结果。
    /// 没有这条，下一个人看到重叠会以为是回归，然后把判据改回块。
    #[test]
    fn symbols_preset_and_emoji_property_deliberately_overlap() {
        let (symbols, _) = BlockMask::from_config(&["符号"]);
        let emoji = emoji_mask();
        // 这几个字符既落在「符号」组的块里，又具有 Unicode 的 Emoji 属性。
        for s in ["▶", "↔", "▪", "〰", "‼"] {
            assert!(symbols.contains_text(s), "{s} 应在「符号」组的块里");
            assert!(emoji.contains_text(s), "{s} 的 Emoji 属性也为真");
        }
    }

    /// ★ 组名不得与块名相同。
    ///
    /// `from_config` 是**先认 emoji、再查组、最后查块**：撞名时块名那一侧静默失效，
    /// 而两者的成员集不同，表现为「配了这个名字，进来的字跟我想的不一样」——没有任何报错。
    ///
    /// ⚠️ `EMOJI_GROUP` 也必须查：它不在 `PRESETS` 里，只遍历 `PRESETS` 会漏掉它，
    /// 而它恰恰是优先级最高、撞名后果最严重的那个名字。
    #[test]
    fn group_names_do_not_collide_with_block_names() {
        assert!(
            !crate::charblock::BLOCKS
                .iter()
                .any(|b| b.name == EMOJI_GROUP),
            "emoji 属性档的名字 {EMOJI_GROUP:?} 与区块表里的块重名"
        );
        for (name, _) in PRESETS {
            assert!(
                !crate::charblock::BLOCKS.iter().any(|b| b.name == *name),
                "预设组名 {name:?} 与区块表里的块重名"
            );
        }
    }

    /// 预设组的成员必须都是真实块名——写错一个字，那一块就静默不进 mask。
    #[test]
    fn preset_members_are_real_block_names() {
        for (group, members) in PRESETS {
            for m in *members {
                assert!(
                    crate::charblock::BLOCKS.iter().any(|b| b.name == *m),
                    "预设组 {group} 的成员 {m:?} 不在区块表里"
                );
            }
        }
    }

    /// ★★ **预设组的覆盖用样本钉住，不核对区间表。**
    ///
    /// 理由与「往返等价」那条判据同形：区间表是显示域的、天然会漏（本轮就补了一块），
    /// 人肉核对区间只能证明「我抄对了自己列的表」。样本测试不必知道规则，直接验结果——
    /// 每一个都是用户真会打出来的形态。
    #[test]
    fn emoji_preset_covers_real_world_samples() {
        let m = emoji_mask();
        for s in [
            "😀", // 表情符号，最常见的那一片
            "⚽️", // U+26BD + FE0F：杂项符号 + 变体选择符
            "👍🏻", // 带肤色修饰符
            "👨‍👩‍👧", // ZWJ 组合家庭
            "🇨🇳", // 区域指示符对
            "✅", // 装饰符号
            "⌚", // 杂项技术符号
            // ↓ 以下全是**块判据漏掉**的，换属性判后必须进来。取自用户实际使用的
            //   五笔 emoji 码表，都是词库里真存在的裸码位形态。
            "🀄",
            "🃏", // 麻将牌 / 扑克牌：整块不在块表里
            "🅰",
            "🆚", // 带圈字母数字补充：旧表只补了尾巴上的区域指示符
            "🈚",
            "🉐", // 带圈表意文字补充
            "⬅",
            "⬛",
            "⭐",
            "⭕", // 杂项符号和箭头
            "⤴",
            "⤵", // 补充箭头 B
            "▶",
            "◽", // 几何图形：整块搬不动，只能按属性判
            "↔",
            "↩",
            "‼",
            "™",
            "ℹ",
            "㊗",
            "Ⓜ",
            "〰",
            "©",
            "®", // 散落在文本块里的
            "❤",
            "☀",
            "✈",
            "🕵", // Emoji=Yes 但默认文本表现，取宽档才留得住
        ] {
            assert!(m.contains_text(s), "{s} 应命中 emoji 组");
        }
    }

    /// 汉字、拉丁字母、中文标点都不该被 emoji 组抓走——免词频若误伤汉字，
    /// 症状是「这个字选了多少次都不往前排」，而且完全静默。
    #[test]
    fn emoji_preset_does_not_catch_text() {
        let m = emoji_mask();
        for s in [
            "我",
            "你好",
            "abc",
            "、",
            "，",
            "１２３",
            "ㄅ",
            "あ",
            "⿰",
            "℃",
            "±",
            "→",
            "■",
            "①",
            "─",
            "∞",
            "龘",
            // ★ 裸数字：`Emoji=Yes` 收了 `0-9#*`（键帽基字符），不额外排除就会让所有
            //   数字候选免词频、还挤进生僻字候选。
            "0",
            "9",
            "#",
            "*",
            "123",
        ] {
            assert!(!m.contains_text(s), "{s} 不应命中 emoji 组");
        }
    }

    /// 键帽序列与 `♠ ☯` 这两处，换判据后各自变成了什么。
    ///
    /// 上一版这条测试叫 `known_imprecisions_are_deliberate`，钉的是两个**不精确**处；
    /// 换成属性判之后一个被修好、一个变成了有据可依的正确结果，故一并改写——留着旧断言
    /// 会让人以为洞还在。
    #[test]
    fn keycap_and_card_suits_after_the_criterion_change() {
        let m = emoji_mask();
        // 键帽：上一版**漏掉**（三个码位没一个在那五块里），现在认得出来。
        // 两种形态都要认——词库里存的往往是省掉 VS16 的那种。
        assert!(m.contains_text("1\u{FE0F}\u{20E3}"), "1️⃣ 完整形态");
        assert!(
            m.contains_text("1\u{20E3}"),
            "1⃣ 省略 VS16，词库里就是这么存的"
        );
        // `♠ ☯`：上一版是「落在杂项符号块里被顺带收进来」的将就，现在是 Unicode 说了算
        // ——它们的 `Emoji` 属性确实为真（`♠` 在 RGI 全表里属 Activities 组）。
        assert!(m.contains_text("♠"));
        assert!(m.contains_text("☯"));
        // 而同块里 `Emoji=No` 的那些，上一版会误收，现在不会。
        assert!(!m.contains_text("☰"), "八卦符号的 Emoji 属性为假");
        assert!(!m.contains_text("⌘"), "命令键符号同理");
    }

    /// 空集必须恒假——这是「没配过的用户零成本」那条路径的正确性前提。
    #[test]
    fn empty_mask_matches_nothing() {
        let m = BlockMask::EMPTY;
        assert!(m.is_empty());
        assert!(!m.contains_text("😀"));
        assert!(!m.contains_char('😀'));
        assert!(!m.contains_text("我"));
        assert_eq!(BlockMask::default(), BlockMask::EMPTY);
    }

    /// 单个块名也能配，且只圈住那一个块。
    #[test]
    fn single_block_name_is_accepted() {
        let (m, unknown) = BlockMask::from_config(&["表情符号"]);
        assert!(unknown.is_empty());
        assert!(m.contains_text("😀"));
        assert!(!m.contains_text("⚽"), "只勾表情符号时不该带上杂项符号块");
    }

    /// 拼错的名字被收集上报，**其余项照常生效**——不因一个错字让整组规则失效。
    #[test]
    fn unknown_names_are_reported_but_do_not_break_the_rest() {
        let (m, unknown) = BlockMask::from_config(&["表情符号", "表情符號", "  ", "emoji"]);
        assert_eq!(unknown, vec!["表情符號".to_string()]);
        assert!(m.contains_text("😀"));
        assert!(m.contains_text("⚽"), "同批次里的 emoji 预设组仍应生效");
    }

    /// 预设组的成员必须都是真实块名——写错一个的后果是它静默地不生效。
    #[test]
    fn preset_members_all_resolve() {
        for (id, members) in PRESETS {
            for m in *members {
                assert!(
                    BLOCKS.iter().any(|b| b.name == *m),
                    "预设组 {id} 的成员 {m} 不是块表里的名字"
                );
            }
        }
    }

    /// 生僻字准入的基本盘：常用字出局、生僻字进来、多字词一律不进。
    #[test]
    fn rare_admits_only_single_uncommon_chars() {
        let cc = CommonChars::from_base(['我', '你', '好', '的']);
        assert!(!rare_admits("我", &cc, BlockMask::EMPTY), "常用字不进");
        assert!(rare_admits("龘", &cc, BlockMask::EMPTY), "生僻汉字要进");
        // 「严格只出单字」（用户 2026-08-24 拍板）：多字词无论常不常用都不进。
        assert!(!rare_admits("你好", &cc, BlockMask::EMPTY));
        assert!(!rare_admits("龘龘", &cc, BlockMask::EMPTY));
        assert!(!rare_admits("", &cc, BlockMask::EMPTY));
        assert!(!rare_admits(" ", &cc, BlockMask::EMPTY), "空白不是字");
    }

    /// ★★★ **取反陷阱**：域外字符（emoji/注音/假名）不会因为「不常用」就自动进来。
    ///
    /// `is_string_common` 对它们恒为 true（语义是「忽略」而非「常用」），所以把准入写成
    /// `!is_string_common` 的话，这些字符会被**恒滤掉**，且代码里找不到任何一处写着要滤
    /// 掉它们——这正是本判据必须是正向白名单的原因。两个方向都钉住：不配 `extra` 时不进，
    /// 配了才进。
    #[test]
    fn out_of_scope_chars_need_explicit_admission() {
        let cc = CommonChars::from_base(['我']);
        for s in ["😀", "ㄅ", "あ", "⿰"] {
            assert!(
                !rare_admits(s, &cc, BlockMask::EMPTY),
                "{s} 未显式纳入时不该进生僻字模式"
            );
        }
        let (emoji, _) = BlockMask::from_config(&["emoji"]);
        assert!(rare_admits("😀", &cc, emoji), "配了 emoji 组就该进");
        assert!(rare_admits("⚽️", &cc, emoji), "带变体选择符的同样要进");
        assert!(!rare_admits("ㄅ", &cc, emoji), "没配的区块仍然不进");
        // 汉字不会因为配了 emoji 组就被带进来（常用字始终出局）。
        assert!(!rare_admits("我", &cc, emoji));
    }

    /// 用户手动设成生僻的字符**直接进**，与它是不是汉字无关。
    ///
    /// 走的是 `is_string_common` 里「覆盖优先于作用域判断」那条既有路径，故这里不必、
    /// 也不该为域外字符再写一遍准入——否则用户在词库管理页把 `、` 设成生僻，
    /// 生僻字模式里却看不到它。
    #[test]
    fn user_marked_rare_chars_are_admitted() {
        let mut cc = CommonChars::from_base(['我', '好']);
        cc.set_overrides([("好".to_string(), false), ("、".to_string(), false)]);
        assert!(
            rare_admits("好", &cc, BlockMask::EMPTY),
            "用户降级的汉字要进"
        );
        assert!(
            rare_admits("、", &cc, BlockMask::EMPTY),
            "用户降级的域外字符同样要进，无需配区块"
        );
        assert!(
            !rare_admits("我", &cc, BlockMask::EMPTY),
            "没表过态的常用字仍出局"
        );
    }

    /// 「一个字」按字素簇算，不按码位数——`⚽️` 是 2 个码位、`👨‍👩‍👧` 是 5 个，都算一个字。
    ///
    /// ⛔ 别退回「跳过修饰码位再数基础字符」那种自己列举 Unicode 规则的写法，
    /// 本仓已经在 `single_markable_char` 那轮走死过一次。
    #[test]
    fn one_char_means_one_grapheme_cluster() {
        let cc = CommonChars::from_base(['我']);
        let (emoji, _) = BlockMask::from_config(&["emoji"]);
        for s in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳"] {
            assert!(rare_admits(s, &cc, emoji), "{s} 是一个字素簇，应算单字");
        }
        assert!(!rare_admits("😀😀", &cc, emoji), "两个字素簇不算单字");
    }

    /// ★ 「其它」兜底档：块表**之外**的字符只有显式配了这一档才命中。
    ///
    /// 存在理由是准入类消费者的失败方向不安全——块表逐块列举一份仍在增长的 Unicode 区间，
    /// 新版本的新块必然落进「其它」。没有这一档的话，那批字在生僻字模式里直接打不出，
    /// 而用户和代码都看不出发生了什么。
    #[test]
    fn other_bucket_catches_chars_outside_the_table() {
        // U+0700（叙利亚字母）不在块表内 —— 用它代表「本程序还不认识的区块」。
        let outside = '\u{0700}';
        assert_eq!(crate::block_of(outside).name, "其它", "样本须真的落在表外");

        let (none, _) = BlockMask::from_config::<&str>(&[]);
        assert!(!none.contains_char(outside), "不配「其它」时不该命中");

        let (other, unknown) = BlockMask::from_config(&["其它"]);
        assert!(unknown.is_empty(), "「其它」必须被认作合法档位而非拼错");
        assert!(other.contains_char(outside), "配了就该命中");

        // 表内字符不会被「其它」捞走——两者是互补的，不是叠加的。
        assert!(!other.contains_char('我'), "基本汉字在表内，不归其它");
        assert!(!other.contains_char('😀'), "表情符号在表内，不归其它");
    }

    /// 「其它」与具名区块可以同时配，互不干扰。
    #[test]
    fn other_bucket_composes_with_named_blocks() {
        let (m, unknown) = BlockMask::from_config(&["emoji", "其它"]);
        assert!(unknown.is_empty());
        assert!(m.contains_char('😀'), "具名组照常生效");
        assert!(m.contains_char('\u{0700}'), "兜底档同时生效");
        assert!(!m.contains_char('我'));
    }
}
