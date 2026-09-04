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
    BLOCKS.len() < 64,
    "块表超出 BlockMask 的位宽：改用 u128，或合并相邻块（最高位留给「其它」兜底档）"
);

/// 「其它」兜底档占用的位——[`OTHER`] 不在 [`BLOCKS`] 里（它是块表**之外**的一切），
/// 没有下标可用，故单独占最高位。位宽断言因此是 `< 64` 而不是 `<= 64`。
///
/// ★ 这一档存在的理由：块表逐块列举一份仍在增长的 Unicode 区间，新版本的新块必然落进
/// 「其它」。对**准入**类消费者（生僻字模式）而言，漏一块 = 那批字打不出——不安全的
/// 方向。给出这一档，新块就落进一个用户控制得到的开关，而不是静默消失。
const OTHER_BIT: u64 = 1 << 63;

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

/// 预设组「符号」：标点、数学、图形这一类**非 emoji** 的符号块。
///
/// ★ 与 [`PRESET_EMOJI`] **刻意不相交**：勾「符号」不该顺带把 emoji 也放进来，否则界面上
/// 两个开关的关系说不清楚（勾了符号，emoji 那个还有什么用）。故「杂项符号」「装饰符号」
/// 「杂项技术符号」归 emoji 组，本组不含——虽然它们名字里也有「符号」二字。
/// 不相交由 `presets_are_disjoint` 钉住。
///
/// ★ 只收 `is_han ∪ is_pua` **域外**的块。部首、康熙部首、CJK 笔画、各扩展区都在
/// `is_han` 里（见 `common::is_han`），本来就是生僻字模式的默认输出，列进来是多余的
/// 开关——用户勾了没变化，只会以为坏了。
pub(crate) const PRESET_SYMBOLS: &[&str] = &[
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
/// ⚠️ 组名与块名同处一个命名空间（`from_config` 先查组、再查块），故**组名不得与任何块名
/// 相同**——否则同一个名字有两种解释，而先查组的写法会让块名那一侧静默失效。
/// 由 `preset_names_do_not_collide_with_block_names` 钉住。
const PRESETS: &[(&str, &[&str])] = &[
    (PRESET_EMOJI_NAME, PRESET_EMOJI),
    (PRESET_SYMBOLS_NAME, PRESET_SYMBOLS),
];

/// 预设组「emoji」的组名。
///
/// ⚠️ 抽成常量是给 [`crate::charset_registry`] 用的：那边**刻意不造**同名内置类
/// （emoji 的成员由 `data/charsets/emoji.yaml` 那份精确字表给出，见
/// `builtin_block_specs` 的文档），而「刻意不造」这件事需要一条测试钉住，
/// 测试得引用同一个字面量才防得住这里改名、那边失配。
pub(crate) const PRESET_EMOJI_NAME: &str = "emoji";

/// 预设组「符号」的组名。[`crate::charset_registry`] 按它建同名内置类。
pub(crate) const PRESET_SYMBOLS_NAME: &str = "符号";

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

    /// 这个字符落在本组内吗。表外字符（[`crate::charblock`] 的「其它」）恒为 false。
    pub fn contains_char(&self, ch: char) -> bool {
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
    pub fn contains_text(&self, text: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        text.chars().any(|c| self.contains_char(c))
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
/// `extra` 就是那道显式纳入：`input.rare_char.include_blocks` 点名的类，以及任何
/// 自己声明了 `in_rare` 的类（两个入口在装配期合并，见 `charset_assembly`）。
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
pub fn rare_admits(text: &str, cc: &crate::CommonChars, extra: &crate::CharsetRegistry) -> bool {
    // 只出单字：多字词、空串、纯空白一律不进。
    let Some(unit) = crate::single_markable_char(text) else {
        return false;
    };
    !cc.is_string_common(unit) || extra.in_rare(unit)
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

    /// ★ 两个预设组**不相交**：勾「符号」不该顺带放进 emoji。
    ///
    /// 名字最容易骗人的三块（杂项符号 / 装饰符号 / 杂项技术符号）字面上都带「符号」，
    /// 归属却在 emoji 组。这条测试是那个归属的唯一书面凭据。
    #[test]
    fn presets_are_disjoint() {
        for (a, ma) in PRESETS {
            for (b, mb) in PRESETS {
                if a == b {
                    continue;
                }
                let shared: Vec<_> = ma.iter().filter(|x| mb.contains(x)).collect();
                assert!(shared.is_empty(), "预设组 {a} 与 {b} 共有成员: {shared:?}");
            }
        }
    }

    /// ★ 组名不得与块名相同。
    ///
    /// `from_config` 是**先查组、再查块**：撞名时块名那一侧静默失效，而两者的成员集不同，
    /// 表现为「配了这个名字，进来的字跟我想的不一样」——没有任何报错。
    #[test]
    fn preset_names_do_not_collide_with_block_names() {
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

    /// 造一个只含一个类、且该类被**点名纳入生僻模式**的 registry。
    ///
    /// 对应 `input.rare_char.include_blocks = [...]` 在 `charset_assembly` 装配之后的
    /// 形态：被点名的类打上了 `in_rare`。测试直接构造判定结构，不经过配置解析——
    /// 「配置能不能走到判据」由 coordinator 侧的
    /// `include_blocks_config_reaches_the_verdict` 端到端钉住，这里只测判据本身。
    fn admitting(members: &[&str]) -> crate::CharsetRegistry {
        let (reg, dropped) = crate::CharsetRegistry::compile(vec![crate::ClassSpec {
            key: "test".into(),
            members: members.iter().map(|s| s.to_string()).collect(),
            in_rare: true,
            ..Default::default()
        }]);
        assert!(dropped.is_empty());
        reg
    }

    /// 谁都没点名的 registry：`in_rare` 恒假，等同于原先的 `BlockMask::EMPTY`。
    fn admitting_none() -> crate::CharsetRegistry {
        crate::CharsetRegistry::default()
    }

    /// 生僻字准入的基本盘：常用字出局、生僻字进来、多字词一律不进。
    #[test]
    fn rare_admits_only_single_uncommon_chars() {
        let cc = CommonChars::from_base(['我', '你', '好', '的']);
        let none = crate::CharsetRegistry::default();
        assert!(!rare_admits("我", &cc, &none), "常用字不进");
        assert!(rare_admits("龘", &cc, &none), "生僻汉字要进");
        // 「严格只出单字」（用户 2026-08-24 拍板）：多字词无论常不常用都不进。
        assert!(!rare_admits("你好", &cc, &none));
        assert!(!rare_admits("龘龘", &cc, &none));
        assert!(!rare_admits("", &cc, &none));
        assert!(!rare_admits(" ", &cc, &none), "空白不是字");
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
        let none = admitting_none();
        for s in ["😀", "ㄅ", "あ", "⿰"] {
            assert!(
                !rare_admits(s, &cc, &none),
                "{s} 未显式纳入时不该进生僻字模式"
            );
        }
        // 字表里只列基字符 `⚽`——`⚽️`（带 U+FE0F）靠逐 char 回落命中，见
        // `CharsetRegistry::cluster_hits`。
        let emoji = admitting(&["😀", "⚽"]);
        assert!(rare_admits("😀", &cc, &emoji), "点名纳入的就该进");
        assert!(rare_admits("⚽️", &cc, &emoji), "带变体选择符的同样要进");
        assert!(!rare_admits("ㄅ", &cc, &emoji), "没纳入的仍然不进");
        // 汉字不会因为纳入了 emoji 就被带进来（常用字始终出局）。
        assert!(!rare_admits("我", &cc, &emoji));
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
        let none = admitting_none();
        assert!(rare_admits("好", &cc, &none), "用户降级的汉字要进");
        assert!(
            rare_admits("、", &cc, &none),
            "用户降级的域外字符同样要进，无需纳入任何类"
        );
        assert!(!rare_admits("我", &cc, &none), "没表过态的常用字仍出局");
    }

    /// 「一个字」按字素簇算，不按码位数——`⚽️` 是 2 个码位、`👨‍👩‍👧` 是 5 个，都算一个字。
    ///
    /// ⛔ 别退回「跳过修饰码位再数基础字符」那种自己列举 Unicode 规则的写法，
    /// 本仓已经在 `single_markable_char` 那轮走死过一次。
    #[test]
    fn one_char_means_one_grapheme_cluster() {
        let cc = CommonChars::from_base(['我']);
        // 只列基字符/首码位：肤色、ZWJ、区域指示符的第二半都靠逐 char 回落命中。
        let emoji = admitting(&["⚽", "👍", "👨", "🇨", "😀"]);
        for s in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳"] {
            assert!(rare_admits(s, &cc, &emoji), "{s} 是一个字素簇，应算单字");
        }
        assert!(!rare_admits("😀😀", &cc, &emoji), "两个字素簇不算单字");
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
