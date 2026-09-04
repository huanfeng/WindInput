//! 生僻字模式的候选准入。
//!
//! # 这里原本还有一个 `BlockMask`
//!
//! 那是「一组 Unicode 区块」的位集，供 `exclude_blocks` / `include_blocks` 做判定。
//! 两个消费者都已切到 [`crate::CharsetRegistry`]，而区块本身也已经**变成配置**
//! （`data/charsets/blocks.yaml`，由 `gen_block_charsets` 从 `charblock::BLOCKS` 生成）
//! ——判定与数据都不在这里了，位集连同它的预设组一并删除。
//!
//! ⚠️ 别把区块表再搬回代码：本次改造的全部目的就是让判据可自定义，而「唯独区块不可配」
//! 与那个目的直接冲突（同一个错误在 emoji 上犯过一次，见设计文档 §5.2）。

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
/// ⚠️ **`extra` 身兼两职**：它同时是 `cc` 做默认判定要问的那份 registry（`common_han`
/// 类给出「是汉字却不在字表里 ⇒ 生僻」）。传一个空 registry 进来，`is_string_common`
/// 会对**所有**字符兜底判常用 ⇒ 生僻汉字也进不来——测试里尤其容易踩，故本文件的
/// 夹具 `registry()` 总是把常用汉字类一并造上。
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
    !cc.is_string_common(unit, extra) || extra.in_rare(unit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommonChars;

    /// 装配之后的真实形态：常用汉字类（给默认判定）+ 被点名纳入生僻模式的类。
    fn registry(base: &[char], admitted: &[&str]) -> crate::CharsetRegistry {
        let mut specs = vec![crate::ClassSpec {
            key: "common_han".into(),
            members: base.iter().map(|c| c.to_string()).collect(),
            scope: Some(crate::Scope::Han),
            default_common: Some(true),
            outside_common: Some(false),
            order: 50,
            ..Default::default()
        }];
        if !admitted.is_empty() {
            specs.push(crate::ClassSpec {
                key: "admitted".into(),
                members: admitted.iter().map(|s| s.to_string()).collect(),
                in_rare: true,
                order: 10,
                ..Default::default()
            });
        }
        let (reg, dropped) = crate::CharsetRegistry::compile(specs);
        assert!(dropped.is_empty());
        reg
    }

    /// 谁都没点名的 registry：常用汉字类照常在，但 `in_rare` 恒假。
    ///
    /// ⚠️ **不能用 `CharsetRegistry::default()`**：`rare_admits` 的第三个参数身兼两职，
    /// 空 registry 会让 `is_string_common` 对所有字符兜底判常用，于是「生僻汉字要进」
    /// 那一半根本测不出来。
    fn admitting_none() -> crate::CharsetRegistry {
        registry(&['我', '你', '好', '的'], &[])
    }

    /// 生僻字准入的基本盘：常用字出局、生僻字进来、多字词一律不进。
    #[test]
    fn rare_admits_only_single_uncommon_chars() {
        let cc = CommonChars::from_base(['我', '你', '好', '的']);
        let none = admitting_none();
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
        let none = registry(&['我'], &[]);
        for s in ["😀", "ㄅ", "あ", "⿰"] {
            assert!(
                !rare_admits(s, &cc, &none),
                "{s} 未显式纳入时不该进生僻字模式"
            );
        }
        // 字表里只列基字符 `⚽`——`⚽️`（带 U+FE0F）靠逐 char 回落命中，见
        // `CharsetRegistry::cluster_hits`。
        let emoji = registry(&['我'], &["😀", "⚽"]);
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
        let none = registry(&['我', '好'], &[]);
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
        let emoji = registry(&['我'], &["⚽", "👍", "👨", "🇨", "😀"]);
        for s in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳"] {
            assert!(rare_admits(s, &cc, &emoji), "{s} 是一个字素簇，应算单字");
        }
        assert!(!rare_admits("😀😀", &cc, &emoji), "两个字素簇不算单字");
    }
}
