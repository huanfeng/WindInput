//! **成组的**字符区块判定：把若干 Unicode 块打成一个集合，供配置驱动的准入/排除使用。
//!
//! [`crate::charblock`] 回答「这一个字符属于哪个块」（单个，用于显示与批量操作）；
//! 本模块回答「这个候选**属不属于这一组块**」（成组，用于判定），并保证判定足够便宜到
//! 可以放上按键热路径。
//!
//! 规划中的两个消费者（本模块先落地，接线在后续提交）：
//!
//! | 消费者 | 用途 | 漏一块的方向 |
//! |---|---|---|
//! | emoji 免词频 | 正常输入里 emoji 不参与词频学习与重排 | 安全（照旧参与） |
//! | 生僻字模式准入 | 哪些区块的字符算「要在该模式里看到的」 | 不安全，须配「其它」兜底档 |
//!
//! 准入判据见 [`crate::charblock`] 模块头那张表——**新增消费者先把自己填进去**。
//!
//! # 为什么是位集而不是 `Vec<CharBlock>`
//!
//! 判定发生在每次按键 × 每个候选上（五笔单字母下 78+ 个候选）。位集把「属于这一组吗」
//! 压成一次移位加一次与运算，且 [`BlockMask`] 是 `Copy` 的，取用时不必克隆、不必借用
//! 配置。配置解析只在装载期做一次。

use crate::charblock::{BLOCKS, block_index_of};

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
    BLOCKS.len() <= 64,
    "块表超出 BlockMask 的位宽：改用 u128，或合并相邻块"
);

/// 预设组：**跨块**的命名集合。
///
/// ★ emoji 不是一个块，是五个——只勾「表情符号」会漏掉 `⚽`(杂项符号)、`✅`(装饰符号)、
/// `⌚`(杂项技术符号)、`🇨🇳`(区域指示符)。用户配置里写 `"emoji"` 指的是整组，而不是
/// 让他自己去拼这五个块名。
///
/// ⚠️ **组里混着非 emoji 的成员**：「杂项符号」块内有 `♠♣☰☯`，「杂项技术符号」块内有
/// `⌘⌥`。这是**刻意接受**的取舍——它们同样是不该参与词频学习的装饰性符号，而要把它们
/// 摘出去就得开始逐字符列举 emoji，正是 `single_markable_char` 那轮已经走死过的路
/// （最终改用 UAX #29 字素簇才收场）。真收到反馈时，把那两块从本组去掉即可，
/// 组的成员是数据、不是判据。
const PRESET_EMOJI: &[&str] = &[
    "表情符号",
    "杂项符号",
    "装饰符号",
    "杂项技术符号",
    "区域指示符",
];

/// 预设组名 → 成员块名。配置里这两种名字都收。
const PRESETS: &[(&str, &[&str])] = &[("emoji", PRESET_EMOJI)];

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
        match BLOCKS.iter().position(|b| b.name == name) {
            Some(i) => {
                self.0 |= 1u64 << i;
                true
            }
            None => false,
        }
    }

    /// 这个字符落在本组内吗。表外字符（[`crate::charblock`] 的「其它」）恒为 false。
    pub fn contains_char(&self, ch: char) -> bool {
        match block_index_of(ch) {
            Some(i) => self.0 & (1u64 << i) != 0,
            None => false,
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
    pub fn contains_text(&self, text: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        text.chars().any(|c| self.contains_char(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emoji_mask() -> BlockMask {
        let (m, unknown) = BlockMask::from_config(&["emoji"]);
        assert!(unknown.is_empty(), "预设组名必须被认得");
        m
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
            "🇨🇳", // 区域指示符对（本轮补的块）
            "✅", // 装饰符号
            "⌚", // 杂项技术符号
        ] {
            assert!(m.contains_text(s), "{s} 应命中 emoji 预设组");
        }
    }

    /// 汉字、拉丁字母、中文标点都不该被 emoji 组抓走——免词频若误伤汉字，
    /// 症状是「这个字选了多少次都不往前排」，而且完全静默。
    #[test]
    fn emoji_preset_does_not_catch_text() {
        let m = emoji_mask();
        for s in ["我", "你好", "abc", "、", "，", "１２３", "ㄅ", "あ", "⿰"] {
            assert!(!m.contains_text(s), "{s} 不应命中 emoji 预设组");
        }
    }

    /// keycap `1️⃣` 与「杂项符号」块内的非 emoji：两个已知的不精确处，**方向都安全**。
    ///
    /// 钉住它们是为了让下一个人知道这是已经想过的取舍，而不是没测到的洞。
    #[test]
    fn known_imprecisions_are_deliberate() {
        let m = emoji_mask();
        // `1️⃣` = '1' + FE0F + U+20E3。三个码位分属 ASCII、变体选择符、组合用记号补充，
        // 没有一个在 emoji 组里 ⇒ 漏掉。方向安全：它照旧参与词频，即改动前的行为。
        assert!(!m.contains_text("1\u{FE0F}\u{20E3}"));
        // 扑克与八卦落在「杂项符号」块内 ⇒ 一并被算作 emoji。见 PRESET_EMOJI 的取舍说明。
        assert!(m.contains_text("♠"));
        assert!(m.contains_text("☯"));
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
}
