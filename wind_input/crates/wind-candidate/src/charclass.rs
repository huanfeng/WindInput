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
#[path = "charclass_tests.rs"]
mod tests;
