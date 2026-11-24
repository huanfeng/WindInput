//! 全拼击键的 ü 拼写归一化（`xv` → `xu`、`nue` → `nve`）。
//!
//! 拼音正字法里 j/q/x/y 后的 ü 写作 `u`，n/l 后保留两点（键盘用 `v`），所以
//! [`syllable::STANDARD_SYLLABLES`](super::syllable::STANDARD_SYLLABLES) 只收 `lv`/`nve` 一类。
//! 用户按习惯打 `xv` 或 `nue`/`lue` 会空码；本模块是双拼侧
//! [`shuangpin::normalize_pinyin`](super::shuangpin) 在全拼域的对应物。
//!
//! ## 两条规则的安全性不对称
//!
//! - `v` → `u`（前一字符 ∈ jqxy）：无损。`v` 在音节表里只跟在 l/n 后，且不能起音节，
//!   jqxy 后的 `v` 怎么切都是死路。
//! - `ue` → `ve`（前一字符 ∈ nl）：有损的取舍。`nue` 本能切成 `nu|e`，但那一族真实输入里
//!   不存在，而 nüe/lüe 是常用字（搜狗/微软同样判给「虐」）。逃生口是手动分隔符 `nu'e`。
//!
//! ## 不变量：等长 1:1 替换
//!
//! 两条规则都只换单个字节、不增删，raw/flat 两域的位置映射（`interp::SylSpan`、
//! `build_raw_preedit`、`consumed_length`）因此不用改。要加增删长度的规则（如 `ng` → `en`）
//! 须先处理这条不变量，不能顺手塞进本函数。

use std::borrow::Cow;

/// 判断字节是否为 j/q/x/y——这四个声母后的 ü 按正字法写作 `u`。
#[inline]
fn is_jqxy(b: u8) -> bool {
    matches!(b, b'j' | b'q' | b'x' | b'y')
}

/// 判断字节是否为 n/l——这两个声母后的 ü 必须保留两点，键盘上写作 `v`。
#[inline]
fn is_nl(b: u8) -> bool {
    matches!(b, b'n' | b'l')
}

/// 把全拼击键串里的 ü 变体拼写归一到音节表使用的正字法形态。
///
/// - `xv` → `xu`、`jvn` → `jun`、`qve` → `que`（jqxy 后的 `v` 即 `u`）
/// - `nue` → `nve`、`lue` → `lve`（n/l 后的 `ue` 即 `üe`）
/// - `nv`/`lv`/`lve` 原样保留（它们已经是音节表的形态）
///
/// 无需替换时返回 [`Cow::Borrowed`]，不分配——本函数在按键热路径上。
///
/// 只接受全拼域的串。双拼击键里 `v` 是布局键（小鹤 `v` = zh 声母），
/// 拿到这里会被替换成 `u`、毁掉整个双拼输入。调用方必须自行确认域。
pub fn normalize_u_umlaut(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    // 快速否定：两条规则分别以 `v`、`u` 为目标，一个都没有就直接借用。
    if !bytes.iter().any(|&b| b == b'v' || b == b'u') {
        return Cow::Borrowed(input);
    }
    // 两条规则的触发条件互斥（前驱集合 jqxy 与 nl 不相交），故可以逐字节独立判断，
    // 且判据取**原串**前驱与后继——替换产物不会连锁触发另一条规则。
    let mut out: Option<Vec<u8>> = None;
    for i in 0..bytes.len() {
        let replacement = match bytes[i] {
            b'v' if i > 0 && is_jqxy(bytes[i - 1]) => b'u',
            b'u' if i > 0 && is_nl(bytes[i - 1]) && bytes.get(i + 1) == Some(&b'e') => b'v',
            _ => continue,
        };
        out.get_or_insert_with(|| bytes.to_vec())[i] = replacement;
    }
    match out {
        // 安全性：只把 ASCII 字节换成 ASCII 字节，UTF-8 边界不受影响。
        Some(v) => Cow::Owned(String::from_utf8(v).expect("ASCII 替换不破坏 UTF-8")),
        None => Cow::Borrowed(input),
    }
}

#[cfg(test)]
#[path = "spelling_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "spelling_exhaustiveness_tests.rs"]
mod exhaustiveness;
