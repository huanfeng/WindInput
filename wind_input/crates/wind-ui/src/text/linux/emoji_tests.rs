use super::*;

fn len(s: &str) -> usize {
    let v: Vec<char> = s.chars().collect();
    cluster_len(&v, 0)
}

#[test]
fn single_emoji_and_plain_text() {
    assert_eq!(len("😀a"), 1);
    assert_eq!(len("a😀"), 0);
    assert_eq!(len("中"), 0);
    assert_eq!(len("1"), 0, "裸数字不是键帽");
    assert_eq!(len("#x"), 0);
}

#[test]
fn skin_tone_and_selectors_join_the_base() {
    assert_eq!(len("👍🏽x"), 2);
    assert_eq!(len("❤\u{FE0F}x"), 2);
    assert_eq!(len("❤x"), 0, "U+2764 默认文本呈现，不带 VS16 走文字");
    assert_eq!(len("😀\u{FE0E}"), 0, "VS15 强制文本呈现");
    assert_eq!(len("⌚"), 1, "BMP 里默认 emoji 呈现的字");
}

#[test]
fn zwj_sequences_are_one_cluster() {
    // 👨‍👩‍👧 = 5 个码位。
    assert_eq!(len("👨\u{200D}👩\u{200D}👧!"), 5);
    // 🏳️‍🌈 = 🏳 VS16 ZWJ 🌈。
    assert_eq!(len("🏳\u{FE0F}\u{200D}🌈"), 4);
    // 带肤色的 ZWJ：👩🏽‍💻。
    assert_eq!(len("👩🏽\u{200D}💻"), 4);
    // 尾随的孤立 ZWJ 不吞。
    assert_eq!(len("😀\u{200D}a"), 1);
}

#[test]
fn keycap_is_looked_up_by_the_keycap_char() {
    assert_eq!(key_char(&['1', VS16, KEYCAP]), KEYCAP);
    assert_eq!(key_char(&['😀']), '😀');
    assert_eq!(first_element_len(&['👩', '🏽', ZWJ, '💻']), 2);
    assert_eq!(first_element_len(&['👍', '🏽']), 2);
}

#[test]
fn flags_keycaps_and_tags() {
    assert_eq!(len("🇨🇳🇺🇸"), 2, "区域指示符两两成对");
    assert_eq!(len("🇨"), 1);
    assert_eq!(len("1\u{FE0F}\u{20E3}2"), 3);
    assert_eq!(len("#\u{20E3}"), 2);
    // 🏴󠁧󠁢󠁳󠁣󠁴󠁿（苏格兰）：🏴 + 5 个标签 + 结束标签。
    assert_eq!(
        len("🏴\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}"),
        7
    );
}
