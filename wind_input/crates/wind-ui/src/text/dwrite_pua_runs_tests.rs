use super::pua_runs;

fn runs(s: &str) -> Vec<(usize, usize)> {
    pua_runs(&s.encode_utf16().collect::<Vec<u16>>())
}

/// 内置 wubi86 拆字库："的" → U+E0E1 U+E124 U+E147 U+E13D（BMP 私用区，单码元）。
#[test]
fn bmp_pua_run_is_detected() {
    assert_eq!(runs("\u{E0E1}\u{E124}\u{E147}\u{E13D}"), vec![(0, 4)]);
}

/// 986 拆字库："的" → U+F00FD U+F00F7 U+F013C（补充私用区 A，各占 2 码元）。
/// 修复前这一段完全不命中，字根落回主字体渲染成方框。
#[test]
fn spua_a_run_is_detected() {
    assert_eq!(runs("\u{F00FD}\u{F00F7}\u{F013C}"), vec![(0, 6)]);
}

/// 补充私用区 B（第 16 平面）同样纳入。
#[test]
fn spua_b_run_is_detected() {
    assert_eq!(runs("\u{100000}\u{10FFFD}"), vec![(0, 4)]);
}

/// 非私用区的代理对不得命中：CJK 扩展 B 等生僻字若被误切到字根字体集会变成方框。
#[test]
fn non_pua_supplementary_chars_are_excluded() {
    assert!(runs("\u{20000}\u{2A6DF}\u{1F600}").is_empty());
}

/// 混排：汉字 + 字根 + 编码，段起止按 UTF-16 码元定位（非字符数）。
/// "的" 1 码元 + "：" 1 码元 → 字根段从下标 2 起、占 6 码元。
#[test]
fn mixed_text_run_offsets_are_utf16_units() {
    assert_eq!(runs("的：\u{F00FD}\u{F00F7}\u{F013C} rqy"), vec![(2, 6)]);
}

/// 多段：被普通字符隔开的字根分别成段。
#[test]
fn separate_runs_are_not_merged_across_plain_text() {
    assert_eq!(runs("\u{E0E1}中\u{F00FD}"), vec![(0, 1), (2, 2)]);
}

/// BMP 与补充私用区相邻时合并为一段（目标字体族相同，合并不改变渲染）。
#[test]
fn adjacent_bmp_and_supplementary_pua_merge() {
    assert_eq!(runs("\u{E0E1}\u{F00FD}"), vec![(0, 3)]);
}

/// 孤立高位代理（非法 UTF-16，可能来自截断的外部数据）不得命中、不得越界 panic。
#[test]
fn lone_lead_surrogate_is_ignored() {
    assert!(pua_runs(&[0xDB80]).is_empty());
    assert!(pua_runs(&[0xDB80, 0x4E2D]).is_empty());
}

#[test]
fn empty_and_plain_text_yield_no_runs() {
    assert!(runs("").is_empty());
    assert!(runs("中文 abc 123").is_empty());
}
