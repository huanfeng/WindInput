//! 物理键盘布局：软键盘的键位集合，以及画布 token 的对位顺序。
//!
//! ★ **软键盘的布局就是键盘的布局**——键位坐标由键名唯一决定，配置里不需要（也不允许）
//! 定义行列。本模块是那份「坐标表」，不是第二套键名体系：这里出现的每个名字都必须能被
//! `wind-keys` 的键名解析认出，由 `tests/key_name_parity.rs` 守门。

/// 画布四行的键位名。**顺序即画布 token 的对位顺序**：第 n 个 token 落到该行第 n 个键位。
///
/// 行的构成对应标准 ANSI 主键区去掉功能键之后剩下的符号键位：
/// 数字行 / QWERTY / ASDF / ZXCV。功能键（Tab / Enter / Shift…）不在此表——
/// 它们是封闭集、硬编码透传，永远不参与映射，见 `docs/design/soft-keyboard.md` §4.2。
pub const KEY_ROWS: [&[&str]; 4] = [
    &[
        "grave", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "minus", "equal",
    ],
    &[
        "q",
        "w",
        "e",
        "r",
        "t",
        "y",
        "u",
        "i",
        "o",
        "p",
        "lbracket",
        "rbracket",
        "backslash",
    ],
    &[
        "a",
        "s",
        "d",
        "f",
        "g",
        "h",
        "j",
        "k",
        "l",
        "semicolon",
        "quote",
    ],
    &[
        "z", "x", "c", "v", "b", "n", "m", "comma", "period", "slash",
    ],
];

/// 键位总数（47）。
pub const SLOT_COUNT: usize = 13 + 13 + 11 + 10;

/// 键名别名 → 规范名。与 `wind-keys` 的 `KEY_TABLE` 同源，只收本布局用得到的符号键。
///
/// 收别名是因为这份文件是给人手写的：用户照着键盘敲 `` ` `` 比记住 `grave` 自然得多。
const ALIASES: &[(&str, &str)] = &[
    ("`", "grave"),
    ("backtick", "grave"),
    ("-", "minus"),
    ("=", "equal"),
    ("equals", "equal"),
    ("[", "lbracket"),
    ("]", "rbracket"),
    ("\\", "backslash"),
    (";", "semicolon"),
    ("'", "quote"),
    (",", "comma"),
    (".", "period"),
    ("/", "slash"),
];

/// 全部键位名，按 [`KEY_ROWS`] 的行列顺序。
pub fn all_slots() -> impl Iterator<Item = &'static str> {
    KEY_ROWS.iter().flat_map(|r| r.iter().copied())
}

/// 规范化键位名：小写 + 别名折叠。不是本布局的键位则返回 `None`。
///
/// 返回 `&'static str` 而非 `String`：调用方拿到的恒是 [`KEY_ROWS`] 里那份，
/// 后续比较与查表都不必再规范化一次。
pub fn normalize_slot(name: &str) -> Option<&'static str> {
    let low = name.trim().to_lowercase();
    if low.is_empty() {
        return None;
    }
    let canon: &str = ALIASES
        .iter()
        .find(|(alias, _)| *alias == low)
        .map(|(_, c)| *c)
        .unwrap_or(&low);
    all_slots().find(|s| *s == canon)
}

/// 拆开补丁键名里的 `shift+` 前缀，返回 `(规范键位名, 是否第二层)`。
///
/// 只认 `shift+` 一个修饰前缀——软键盘只有两层，别的修饰键在软键盘态下走透传，
/// 收下它们只会让「配了没反应」多一种成因。
pub fn parse_patch_key(name: &str) -> Option<(&'static str, bool)> {
    let t = name.trim();
    let (rest, shift) = match t
        .strip_prefix("shift+")
        .or_else(|| t.strip_prefix("Shift+"))
        .or_else(|| t.strip_prefix("SHIFT+"))
    {
        Some(r) => (r, true),
        None => (t, false),
    };
    normalize_slot(rest).map(|s| (s, shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_count_matches_rows() {
        assert_eq!(all_slots().count(), SLOT_COUNT);
    }

    #[test]
    fn slots_are_unique() {
        let mut seen: Vec<&str> = all_slots().collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "键位名重复");
    }

    #[test]
    fn normalize_accepts_aliases_and_case() {
        assert_eq!(normalize_slot("`"), Some("grave"));
        assert_eq!(normalize_slot("BackTick"), Some("grave"));
        assert_eq!(normalize_slot(" Q "), Some("q"));
        assert_eq!(normalize_slot("["), Some("lbracket"));
        assert_eq!(normalize_slot("equals"), Some("equal"));
        assert_eq!(normalize_slot("enter"), None, "功能键不是可映射键位");
        assert_eq!(normalize_slot(""), None);
    }

    #[test]
    fn patch_key_splits_shift() {
        assert_eq!(parse_patch_key("q"), Some(("q", false)));
        assert_eq!(parse_patch_key("shift+q"), Some(("q", true)));
        assert_eq!(parse_patch_key("Shift+["), Some(("lbracket", true)));
        assert_eq!(parse_patch_key("ctrl+q"), None, "只认 shift 前缀");
        assert_eq!(parse_patch_key("shift+enter"), None);
    }
}
