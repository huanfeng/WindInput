//! 最小 emoji 序列切分：把文本里的一个 emoji 序列（肤色修饰、ZWJ 组合、国旗区域指示符对、
//! 键帽、变体选择符、子区旗标签）聚成一个簇，交给彩色字体整簇整形。
//!
//! 不是 UTS #51 的完整实现：不带 Unicode 的 `Extended_Pictographic` / `Emoji_Presentation`
//! 属性表，用区段近似（见 [`is_pictographic`] / [`default_emoji_presentation`]）。近似的
//! 代价只落在「默认文本呈现的符号该不该画成彩色」这一小撮字上；带了 VS16 的一律按彩色，
//! 带 VS15 的一律按文本，这两条是精确的。

/// 变体选择符 VS15（文本呈现）。
const VS15: char = '\u{FE0E}';
/// 变体选择符 VS16（emoji 呈现）。
const VS16: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';
/// 组合用键帽 U+20E3。
const KEYCAP: char = '\u{20E3}';

fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// 肤色修饰符（Fitzpatrick 1-2 … 6）。
fn is_skin_tone(c: char) -> bool {
    matches!(c as u32, 0x1F3FB..=0x1F3FF)
}

/// 子区旗的标签字符（🏴 + 标签序列 + U+E007F）。
fn is_tag(c: char) -> bool {
    matches!(c as u32, 0xE0020..=0xE007F)
}

/// 簇里不占位、字体可以不收录的成分：变体选择符、ZWJ、标签字符。整形后它们落成 0 号字形
/// 是正常的，丢掉即可；别的字落成 0 号字形则是字体真缺字。
pub(super) fn is_invisible_component(c: char) -> bool {
    c == VS15 || c == VS16 || c == ZWJ || is_tag(c)
}

/// ZWJ 组合簇里第一个元素的长度（到第一个 ZWJ 为止）；不含 ZWJ 的簇返回整簇长度。
/// 字体缺组合里某个成分时，先把第一个元素单独画成彩色，剩下的接着切簇。
pub(super) fn first_element_len(cluster: &[char]) -> usize {
    cluster
        .iter()
        .position(|&c| c == ZWJ)
        .unwrap_or(cluster.len())
}

/// 查字体时用哪个字代表整簇：键帽簇用 U+20E3（数字、`#`、`*` 几乎每款字体都有，拿它们
/// 去找彩色字体会把整条回退链都加载一遍），其余用首字。
pub(super) fn key_char(cluster: &[char]) -> char {
    match cluster.last() {
        Some(&KEYCAP) => KEYCAP,
        _ => cluster[0],
    }
}

/// `Extended_Pictographic` 的区段近似：能作 emoji 序列起点、能接在 ZWJ 后面的字。
fn is_pictographic(c: char) -> bool {
    matches!(
        c as u32,
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x2199
            | 0x21A9..=0x21AA
            | 0x231A..=0x231B
            | 0x2328
            | 0x23CF
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x24C2
            | 0x25AA..=0x25AB
            | 0x25B6
            | 0x25C0
            | 0x25FB..=0x25FE
            | 0x2600..=0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
            | 0x1FC00..=0x1FFFD
    )
}

/// `Emoji_Presentation` 的近似：不带 VS16 也默认画成彩色的字。补充平面的图形区几乎全是；
/// BMP 里只有下面这一小撮（其余如 ☺ ♥ ✌ 默认按文本呈现，要带 VS16 才是彩色）。
fn default_emoji_presentation(c: char) -> bool {
    let u = c as u32;
    if u >= 0x1F000 {
        return is_pictographic(c);
    }
    matches!(
        u,
        0x231A..=0x231B
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
    )
}

/// 一个 emoji 元素后面可以挂的修饰（变体选择符、肤色、标签、键帽），返回吃掉后的下标与
/// 其中见没见到 VS15 / 「强制彩色」的修饰。
fn eat_tail(s: &[char], mut j: usize) -> (usize, bool, bool) {
    let (mut text, mut emoji) = (false, false);
    while let Some(&c) = s.get(j) {
        if c == VS15 {
            text = true;
        } else if c == VS16 || is_skin_tone(c) || is_tag(c) || c == KEYCAP {
            emoji = true;
        } else {
            break;
        }
        j += 1;
    }
    (j, text, emoji)
}

/// 从 `s[i]` 起的 emoji 序列长度（按字符计）；`0` = 这里不是一个要画成彩色的 emoji 序列
/// （不是 emoji、带 VS15、或默认文本呈现又没带 VS16），调用方照逐字排。
pub(super) fn cluster_len(s: &[char], i: usize) -> usize {
    let Some(&c) = s.get(i) else {
        return 0;
    };
    // 国旗：两个区域指示符成对；落单的一个也是 emoji 呈现（字体会给个带框字母）。
    if is_regional_indicator(c) {
        return if s.get(i + 1).copied().is_some_and(is_regional_indicator) {
            2
        } else {
            1
        };
    }
    // 键帽：[0-9#*] (VS16)? U+20E3。不带 U+20E3 的数字与 # * 是普通文字。
    if c.is_ascii_digit() || c == '#' || c == '*' {
        let mut j = i + 1;
        if s.get(j) == Some(&VS16) {
            j += 1;
        }
        return if s.get(j) == Some(&KEYCAP) {
            j + 1 - i
        } else {
            0
        };
    }
    if !is_pictographic(c) {
        return 0;
    }
    let (mut j, text, mut emoji) = eat_tail(s, i + 1);
    // ZWJ 组合：ZWJ 后面还得是个图形字才接上（孤零零的 ZWJ 不吞，照旧是零宽格式字符）。
    while s.get(j) == Some(&ZWJ) && s.get(j + 1).copied().is_some_and(is_pictographic) {
        emoji = true;
        (j, _, _) = eat_tail(s, j + 2);
    }
    if emoji || (!text && default_emoji_presentation(c)) {
        j - i
    } else {
        0
    }
}

#[cfg(test)]
#[path = "emoji_tests.rs"]
mod tests;
