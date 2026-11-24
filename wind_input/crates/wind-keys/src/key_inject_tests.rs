use super::*;

/// key.type 的换行必须变成**按键**。作为字符投递时多数控件不换行，
/// 用户写的换行就无声无息消失了。
#[test]
fn type_text_splits_newline_into_key() {
    let nl = char::from_u32(10).unwrap();
    let text = format!("第一行{nl}第二行");
    assert_eq!(
        split_type_text(&text),
        vec![
            TypeChunk::Text("第一行"),
            TypeChunk::Key(0x0D),
            TypeChunk::Text("第二行"),
        ]
    );
}

#[test]
fn type_text_splits_tab_into_key() {
    let tab = char::from_u32(9).unwrap();
    let text = format!("a{tab}b");
    assert_eq!(
        split_type_text(&text),
        vec![
            TypeChunk::Text("a"),
            TypeChunk::Key(0x09),
            TypeChunk::Text("b")
        ]
    );
}

/// CRLF 是一个换行的两种写法，逐个转按键会敲出两行。
#[test]
fn crlf_is_one_return() {
    let cr = char::from_u32(13).unwrap();
    let nl = char::from_u32(10).unwrap();
    let text = format!("a{cr}{nl}b");
    assert_eq!(
        split_type_text(&text),
        vec![
            TypeChunk::Text("a"),
            TypeChunk::Key(0x0D),
            TypeChunk::Text("b")
        ]
    );
}

#[test]
fn lone_cr_is_also_return() {
    let cr = char::from_u32(13).unwrap();
    let text = format!("a{cr}b");
    assert_eq!(
        split_type_text(&text),
        vec![
            TypeChunk::Text("a"),
            TypeChunk::Key(0x0D),
            TypeChunk::Text("b")
        ]
    );
}

#[test]
fn plain_text_is_one_chunk() {
    assert_eq!(
        split_type_text("Hello, 世界"),
        vec![TypeChunk::Text("Hello, 世界")]
    );
    assert_eq!(split_type_text(""), vec![]);
}

/// 连续换行不合并（两个空行就是两次回车），且首尾不产出空文本段。
#[test]
fn consecutive_and_edge_newlines() {
    let nl = char::from_u32(10).unwrap();
    let text = format!("{nl}a{nl}{nl}b{nl}");
    assert_eq!(
        split_type_text(&text),
        vec![
            TypeChunk::Key(0x0D),
            TypeChunk::Text("a"),
            TypeChunk::Key(0x0D),
            TypeChunk::Key(0x0D),
            TypeChunk::Text("b"),
            TypeChunk::Key(0x0D),
        ]
    );
}

/// 多字节字符紧贴控制字符：切片边界必须落在字符边界上，否则 panic。
#[test]
fn multibyte_next_to_control_char_does_not_panic() {
    let nl = char::from_u32(10).unwrap();
    let text = format!("知典{nl}超精");
    assert_eq!(
        split_type_text(&text),
        vec![
            TypeChunk::Text("知典"),
            TypeChunk::Key(0x0D),
            TypeChunk::Text("超精"),
        ]
    );
}

/// 其余控制字符不动（维持现状，不顺手改变）。
#[test]
fn other_control_chars_stay_in_text() {
    let bell = char::from_u32(7).unwrap();
    let text = format!("a{bell}b");
    assert_eq!(split_type_text(&text), vec![TypeChunk::Text(text.as_str())]);
}

#[test]
fn parse_simple_keys() {
    assert_eq!(parse_combo("Enter"), Some((vec![], 0x0D)));
    assert_eq!(parse_combo("left"), Some((vec![], 0x25)));
    assert_eq!(parse_combo("F5"), Some((vec![], 0x74)));
    assert_eq!(parse_combo("3"), Some((vec![], 0x33)));
    assert_eq!(parse_combo("a"), Some((vec![], 0x41)));
}

#[test]
fn parse_with_modifiers() {
    assert_eq!(parse_combo("Ctrl+C"), Some((vec![VK_CONTROL], 0x43)));
    assert_eq!(
        parse_combo("Ctrl+Shift+End"),
        Some((vec![VK_CONTROL, VK_SHIFT], 0x23))
    );
    assert_eq!(parse_combo("Alt+Tab"), Some((vec![VK_MENU], 0x09)));
    assert_eq!(parse_combo("Win+d"), Some((vec![VK_LWIN], 0x44)));
}

#[test]
fn parse_raw_vk() {
    assert_eq!(parse_combo("vk:0x5D"), Some((vec![], 0x5D)));
    assert_eq!(parse_combo("Ctrl+vk:0x42"), Some((vec![VK_CONTROL], 0x42)));
}

#[test]
fn parse_invalid() {
    assert_eq!(parse_combo("Bogus+X"), None); // 未知修饰键
    assert_eq!(parse_combo(""), None);
    assert_eq!(parse_combo("nonsuchkey"), None);
}

#[test]
fn vk_to_cgkeycode_modifiers() {
    assert_eq!(vk_to_cgkeycode(0x10), Some(56)); // Shift
    assert_eq!(vk_to_cgkeycode(0x11), Some(59)); // Control
    assert_eq!(vk_to_cgkeycode(0x12), Some(58)); // Option(Alt)
    assert_eq!(vk_to_cgkeycode(0x5B), Some(55)); // Command(LWin)
}

#[test]
fn vk_to_cgkeycode_function_keys() {
    assert_eq!(vk_to_cgkeycode(0x0D), Some(36)); // Return
    assert_eq!(vk_to_cgkeycode(0x09), Some(48)); // Tab
    assert_eq!(vk_to_cgkeycode(0x1B), Some(53)); // Escape
    assert_eq!(vk_to_cgkeycode(0x20), Some(49)); // Space
    assert_eq!(vk_to_cgkeycode(0x08), Some(51)); // Backspace -> Delete
    assert_eq!(vk_to_cgkeycode(0x2E), Some(117)); // Delete -> ForwardDelete
    assert_eq!(vk_to_cgkeycode(0x24), Some(115)); // Home
    assert_eq!(vk_to_cgkeycode(0x23), Some(119)); // End
    assert_eq!(vk_to_cgkeycode(0x21), Some(116)); // PageUp
    assert_eq!(vk_to_cgkeycode(0x22), Some(121)); // PageDown
    assert_eq!(vk_to_cgkeycode(0x25), Some(123)); // Left
    assert_eq!(vk_to_cgkeycode(0x26), Some(126)); // Up
    assert_eq!(vk_to_cgkeycode(0x27), Some(124)); // Right
    assert_eq!(vk_to_cgkeycode(0x28), Some(125)); // Down
}

#[test]
fn vk_to_cgkeycode_f_keys() {
    assert_eq!(vk_to_cgkeycode(0x70), Some(122)); // F1
    assert_eq!(vk_to_cgkeycode(0x71), Some(120)); // F2
    assert_eq!(vk_to_cgkeycode(0x72), Some(99)); // F3
    assert_eq!(vk_to_cgkeycode(0x73), Some(118)); // F4
    assert_eq!(vk_to_cgkeycode(0x74), Some(96)); // F5
    assert_eq!(vk_to_cgkeycode(0x75), Some(97)); // F6
    assert_eq!(vk_to_cgkeycode(0x76), Some(98)); // F7
    assert_eq!(vk_to_cgkeycode(0x77), Some(100)); // F8
    assert_eq!(vk_to_cgkeycode(0x78), Some(101)); // F9
    assert_eq!(vk_to_cgkeycode(0x79), Some(109)); // F10
    assert_eq!(vk_to_cgkeycode(0x7A), Some(103)); // F11
    assert_eq!(vk_to_cgkeycode(0x7B), Some(111)); // F12
}

#[test]
fn vk_to_cgkeycode_letters() {
    assert_eq!(vk_to_cgkeycode(0x41), Some(0)); // A
    assert_eq!(vk_to_cgkeycode(0x53), Some(1)); // S
    assert_eq!(vk_to_cgkeycode(0x5A), Some(6)); // Z
    assert_eq!(vk_to_cgkeycode(0x4D), Some(46)); // M
    assert_eq!(vk_to_cgkeycode(0x51), Some(12)); // Q
    assert_eq!(vk_to_cgkeycode(0x50), Some(35)); // P
}

#[test]
fn vk_to_cgkeycode_digits() {
    assert_eq!(vk_to_cgkeycode(0x31), Some(18)); // 1
    assert_eq!(vk_to_cgkeycode(0x32), Some(19)); // 2
    assert_eq!(vk_to_cgkeycode(0x35), Some(23)); // 5
    assert_eq!(vk_to_cgkeycode(0x36), Some(22)); // 6
    assert_eq!(vk_to_cgkeycode(0x37), Some(26)); // 7
    assert_eq!(vk_to_cgkeycode(0x39), Some(25)); // 9
    assert_eq!(vk_to_cgkeycode(0x30), Some(29)); // 0
}

#[test]
fn vk_to_cgkeycode_oem_symbols() {
    // 出厂默认 keys.activate_ime = "ctrl+shift+[" 就落在 OEM_4 上：漏了它，
    // macOS 的「切换到本输入法」热键开箱即注册不上（只有一条 warn）。
    assert_eq!(vk_to_cgkeycode(0xDB), Some(33)); // [
    assert_eq!(vk_to_cgkeycode(0xDD), Some(30)); // ]
    assert_eq!(vk_to_cgkeycode(0xBA), Some(41)); // ;
    assert_eq!(vk_to_cgkeycode(0xC0), Some(50)); // `
    assert_eq!(vk_to_cgkeycode(0xBF), Some(44)); // /
    assert_eq!(vk_to_cgkeycode(0xDE), Some(39)); // '
}

#[test]
fn vk_to_cgkeycode_unknown() {
    assert_eq!(vk_to_cgkeycode(0xFFFF), None);
    assert_eq!(vk_to_cgkeycode(0x00), None);
    assert_eq!(vk_to_cgkeycode(0x91), None); // ScrollLock，本表不覆盖
}
