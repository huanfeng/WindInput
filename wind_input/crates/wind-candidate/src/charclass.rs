//! 生僻字模式的候选准入。
//!
//! 区块判定已在 [`crate::CharsetRegistry`]，数据是 `data/charsets/blocks.yaml`。别把区块表
//! 搬回代码：判据可自定义是那次改造的目的（见设计文档 §5.2）。

/// 生僻字模式的候选准入：这条候选要不要出现在生僻字模式的列表里。
///
/// 判据不是 `!is_common`：[`crate::CommonChars::is_string_common`] 对默认字表管辖域之外的
/// 字符（emoji、注音、假名）恒返回 `true`，意为「忽略」而非「常用」，取反会把它们全部滤掉。
/// 故准入写成正向白名单：
///
/// | 字符 | `is_string_common` | 进不进 |
/// |---|---|---|
/// | 生僻汉字（域内、不在默认表） | false | ✅ |
/// | 常用汉字 | true | ❌ |
/// | 用户手动设成生僻的任何字符 | false（覆盖优先） | ✅ |
/// | emoji / 注音 / 假名（未表态） | true（被忽略） | 只有 `extra` 圈中才进 |
///
/// `extra` 是显式纳入：`input.rare_char.include_blocks` 点名的类与声明了 `in_rare` 的类
/// （装配期合并，见 `charset_assembly`）。它同时也是 `cc` 做默认判定要问的 registry：传空
/// registry 会让生僻汉字也进不来，测试夹具 `registry()` 因此总带上常用汉字类。
///
/// 只出单字，判「一个字」走 [`crate::single_markable_char`]（UAX #29 字素簇），别自己数码位。
/// 本函数刻意不知道模式如何进入（无 `State` / `ModeKind`），以便别的入口原样复用。
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
