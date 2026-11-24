//! 中英 / 中数交界补空格的纯判据（设计见 `docs/design/auto-spacing-and-latin-smart-punct.md`）。
//!
//! 只回答「前一字符是 X、本笔首字符是 Y，要不要在中间补一个空格」，不碰状态、不碰配置结构：
//! 「前一字符」从哪来（客户端输入序列 / 服务端上一笔上屏）是调用方的事。
//!
//! 只管**切换处**：连续的英文之间不补（英文词的词间空格由英文上屏时按 `commit_space` 立即补）。

/// 字符在补空格判据里的类别。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CharClass {
    /// 汉字、假名、注音、CJK 部首与带圈字符。韩文不算：韩文本身词间有空格，混排习惯不同。
    Cjk,
    /// 拉丁字母（ASCII 与拉丁扩展，不含全角字母）。
    Latin,
    /// ASCII 数字。全角数字归 [`Self::WideLatin`]。
    Digit,
    /// 全角字母与全角数字（`ｈｅｌｌｏ`、`１２`）：不参与中英交界——全角态下的排版本来就不在
    /// 中英之间补空格。
    WideLatin,
    /// 空白、标点、符号、emoji、不可知。
    #[default]
    Other,
}

fn is_cjk(cp: u32) -> bool {
    matches!(
        cp,
        0x2E80..=0x2FDF      // CJK 部首、康熙部首
            | 0x3040..=0x309F // 平假名
            | 0x30A0..=0x30FA // 片假名（不含中点 30FB）
            | 0x30FC..=0x30FF
            | 0x3100..=0x312F // 注音
            | 0x31F0..=0x31FF // 片假名音标扩展
            | 0x3200..=0x32FF // 带圈 CJK
            | 0x3400..=0x4DBF // 扩展 A
            | 0x4E00..=0x9FFF // 基本区
            | 0xF900..=0xFAFF // 兼容汉字
            | 0x20000..=0x3FFFF // 扩展 B 起的 2、3 号平面
    )
}

/// 按字符分类。
pub fn classify(c: char) -> CharClass {
    let cp = c as u32;
    if c.is_ascii_digit() {
        CharClass::Digit
    } else if c.is_ascii_alphabetic() || (matches!(cp, 0xC0..=0x24F) && cp != 0xD7 && cp != 0xF7) {
        CharClass::Latin
    } else if matches!(cp, 0xFF10..=0xFF19 | 0xFF21..=0xFF3A | 0xFF41..=0xFF5A) {
        CharClass::WideLatin
    } else if is_cjk(cp) {
        CharClass::Cjk
    } else {
        CharClass::Other
    }
}

/// 按客户端上报的 `seq_char`（UTF-16 单元，0 = 不可知）分类。
///
/// 客户端对辅助平面字符报的是**高代理**：`D840..=D8BF` 正好是 2、3 号平面（扩展 B 起的汉字），
/// 其余代理（emoji 所在的 1 号平面等）归 `Other`。
pub fn classify_unit(u: u16) -> CharClass {
    match u {
        0 => CharClass::Other,
        0xD840..=0xD8BF => CharClass::Cjk,
        0xD800..=0xDFFF => CharClass::Other,
        _ => char::from_u32(u as u32).map_or(CharClass::Other, classify),
    }
}

/// 这一处交界要不要补空格。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpacingRules {
    /// 汉字 ↔ 拉丁字母（`input.spacing.cjk_latin`）。
    pub cjk_latin: bool,
    /// 汉字 ↔ 数字（`input.spacing.cjk_digit`）。
    pub cjk_digit: bool,
}

/// `prev` = 插入点前一字符的类别，`next` = 本笔上屏文本首字符的类别。
pub fn needs_space(prev: CharClass, next: CharClass, rules: SpacingRules) -> bool {
    use CharClass::*;
    match (prev, next) {
        (Cjk, Latin) | (Latin, Cjk) => rules.cjk_latin,
        (Cjk, Digit) | (Digit, Cjk) => rules.cjk_digit,
        _ => false,
    }
}

#[cfg(test)]
#[path = "spacing_tests.rs"]
mod tests;
