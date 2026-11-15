//! VK → 物理键名与分类，供扩展事件使用（`wind-hook` 的 `KeyEvent.key` / `class`）。
//!
//! 键名一律小写、与产出字符无关：`a` 就是 A 键，不管当时打出的是 `a`、`A` 还是汉字的编码。
//! 符号键复用 [`crate::keymap`] 里引导键配置用的规范名，两边说的是同一套名字。

use crate::keymap::*;

const LETTERS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];
const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
const FKEYS: [&str; 24] = [
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13", "f14", "f15",
    "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24",
];
const NUMPAD: [&str; 10] = [
    "numpad0", "numpad1", "numpad2", "numpad3", "numpad4", "numpad5", "numpad6", "numpad7",
    "numpad8", "numpad9",
];

fn index(vk: u32, lo: u32, hi: u32) -> Option<usize> {
    (lo..=hi).contains(&vk).then(|| (vk - lo) as usize)
}

/// VK → 物理键名；不在表里的键返回 `None`。
pub fn vk_key_name(vk: u32) -> Option<&'static str> {
    if let Some(i) = index(vk, VK_A, VK_Z) {
        return Some(LETTERS[i]);
    }
    if let Some(i) = index(vk, VK_0, VK_9) {
        return Some(DIGITS[i]);
    }
    if let Some(i) = index(vk, VK_F1, VK_F24) {
        return Some(FKEYS[i]);
    }
    if let Some(i) = index(vk, VK_NUMPAD0, VK_NUMPAD9) {
        return Some(NUMPAD[i]);
    }
    if let Some(n) = vk_to_symbol_key_name(vk) {
        return Some(n);
    }
    Some(match vk {
        VK_SPACE => "space",
        VK_RETURN => "enter",
        VK_TAB => "tab",
        VK_BACK => "backspace",
        VK_ESCAPE => "escape",
        VK_DELETE => "delete",
        VK_INSERT => "insert",
        VK_HOME => "home",
        VK_END => "end",
        VK_PRIOR => "pageup",
        VK_NEXT => "pagedown",
        VK_LEFT => "left",
        VK_RIGHT => "right",
        VK_UP => "up",
        VK_DOWN => "down",
        VK_MULTIPLY => "numpad_multiply",
        VK_ADD => "numpad_add",
        VK_SUBTRACT => "numpad_subtract",
        VK_DECIMAL => "numpad_decimal",
        VK_DIVIDE => "numpad_divide",
        VK_CAPITAL => "capslock",
        VK_LSHIFT => "lshift",
        VK_RSHIFT => "rshift",
        VK_LCONTROL => "lctrl",
        VK_RCONTROL => "rctrl",
        _ => return None,
    })
}

/// VK → 分类。按键音这类只关心「打的是哪一类键」的扩展用它。
pub fn vk_key_class(vk: u32) -> &'static str {
    if (VK_A..=VK_Z).contains(&vk) {
        return "letter";
    }
    if (VK_0..=VK_9).contains(&vk) {
        return "digit";
    }
    if (VK_NUMPAD0..=VK_DIVIDE).contains(&vk) {
        return "numpad";
    }
    if (VK_F1..=VK_F24).contains(&vk) {
        return "function";
    }
    if vk_to_symbol_key_name(vk).is_some() {
        return "punct";
    }
    match vk {
        VK_SPACE => "space",
        VK_RETURN => "enter",
        VK_BACK => "backspace",
        VK_TAB | VK_ESCAPE | VK_DELETE | VK_INSERT => "edit",
        VK_HOME | VK_END | VK_PRIOR | VK_NEXT | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
            "navigation"
        }
        VK_CAPITAL | VK_LSHIFT | VK_RSHIFT | VK_LCONTROL | VK_RCONTROL => "modifier",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cover_main_keyboard() {
        assert_eq!(vk_key_name(VK_A), Some("a"));
        assert_eq!(vk_key_name(VK_Z), Some("z"));
        assert_eq!(vk_key_name(VK_9), Some("9"));
        assert_eq!(vk_key_name(VK_SEMICOLON), Some("semicolon"));
        assert_eq!(vk_key_name(VK_SPACE), Some("space"));
        assert_eq!(vk_key_name(VK_F1 + 11), Some("f12"));
        assert_eq!(vk_key_name(VK_NUMPAD0 + 5), Some("numpad5"));
        assert_eq!(vk_key_name(0xFF), None);
    }

    #[test]
    fn classes() {
        assert_eq!(vk_key_class(VK_A + 3), "letter");
        assert_eq!(vk_key_class(VK_1), "digit");
        assert_eq!(vk_key_class(VK_COMMA), "punct");
        assert_eq!(vk_key_class(VK_RETURN), "enter");
        assert_eq!(vk_key_class(VK_BACK), "backspace");
        assert_eq!(vk_key_class(VK_LEFT), "navigation");
        assert_eq!(vk_key_class(VK_ADD), "numpad");
        assert_eq!(vk_key_class(0xFF), "other");
    }

    /// 键名不能重复：扩展按名字配音效，重名就分不清是哪个键。
    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashMap::new();
        for vk in 0..=0xFFu32 {
            if let Some(n) = vk_key_name(vk) {
                assert!(seen.insert(n, vk).is_none(), "键名 {n} 重复");
            }
        }
    }
}
