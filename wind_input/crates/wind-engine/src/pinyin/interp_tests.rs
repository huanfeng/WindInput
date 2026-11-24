use super::*;

fn trie() -> SyllableTrie {
    SyllableTrie::new()
}

/// 无分隔符时 raw 与 flat 完全重合（零回归红线：这是最常见的输入形态）。
#[test]
fn spans_without_separator_are_identity() {
    let s = spans_for_full_pinyin("nihao", &trie());
    assert_eq!(s.len(), 2, "ni|hao: {s:?}");
    assert_eq!((s[0].raw_start, s[0].raw_end), (0, 2));
    assert_eq!((s[0].fp_start, s[0].fp_end), (0, 2));
    assert_eq!((s[1].raw_start, s[1].raw_end), (2, 5));
    assert_eq!((s[1].fp_start, s[1].fp_end), (2, 5));
}

/// 分隔符占 raw 不占 flat —— 两域自此错开一个字节。
#[test]
fn separator_shifts_raw_but_not_flat() {
    let s = spans_for_full_pinyin("xi'an", &trie());
    assert_eq!(s.len(), 2, "xi|an: {s:?}");
    assert_eq!((s[0].raw_start, s[0].raw_end), (0, 2));
    assert_eq!((s[0].fp_start, s[0].fp_end), (0, 2));
    // an 在 raw 里从 3 起（跨过 '），在 flat 里从 2 起
    assert_eq!((s[1].raw_start, s[1].raw_end), (3, 5));
    assert_eq!((s[1].fp_start, s[1].fp_end), (2, 4));
}

/// 段内残码在两域同步跳过，后续段不得错位。
#[test]
fn trailing_garbage_keeps_later_segments_aligned() {
    // 第二段 anx 只切出 an，x 是残码；第三段 hao 的偏移仍须正确
    let s = spans_for_full_pinyin("xi'anx'hao", &trie());
    let hao = s.last().expect("应切出 hao");
    assert_eq!(hao.pinyin, "hao");
    // raw: xi(0..2) '(2) anx(3..6) '(6) hao(7..10)
    assert_eq!((hao.raw_start, hao.raw_end), (7, 10));
    // flat: xi(0..2) anx(2..5) hao(5..8) —— 残码 x 同样占 flat 一个字节
    assert_eq!((hao.fp_start, hao.fp_end), (5, 8));
}

/// 钉住分隔符边缘形态下 flat→raw 的映射（逐字节数分隔符的语义，按音节 span 定位求得）。
#[test]
fn map_fp_to_raw_matches_replaced_byte_scanner() {
    let t = trie();
    let m = |raw: &str, fp: usize| map_fp_to_raw(&spans_for_full_pinyin(raw, &t), fp, raw);

    // 无分隔符：恒等（零回归红线）
    assert_eq!(m("nihao", 0), 0);
    assert_eq!(m("nihao", 2), 2);
    assert_eq!(m("nihao", 5), 5);
    // xi'an：「西安」code="xian" 消费 query 4 → 含 ' 的原始空间 5（全消费）
    assert_eq!(m("xi'an", 4), 5);
    // xi'an：「西」code="xi" 消费 2 → 边界紧跟 ' 归入已消费侧 → 3（残留 "an" 而非 "'an"）
    assert_eq!(m("xi'an", 2), 3);
    // 连续 '' 一并吸收：ni''hao 消费 "ni"(2) → 吃掉两个 ' → 4（残留 "hao"）
    assert_eq!(m("ni''hao", 2), 4);
    // 末尾 '：ni' 全消费 2 → 吸收尾部 ' → 3
    assert_eq!(m("ni'", 2), 3);
    // nih'ao：段内残码 h 不成音节；消费 "ni"(2) 时 h 非分隔符不吸收 → 2，残留 "h'ao"
    assert_eq!(m("nih'ao", 2), 2);
    // nih'ao 全 query 消费 5 → 覆盖到末尾 6
    assert_eq!(m("nih'ao", 5), 6);
}

/// span 的 flat 总长恒等于剥除 `'` 后的串长 —— 这是「映射永不越界」的依据，
/// 也是残码要占 flat 位的理由。破了它，`map_fp_to_raw` 就会掉进防御性续推那一支。
#[test]
fn flat_span_covers_whole_query() {
    for raw in ["nihao", "xi'an", "ni''hao", "ni'", "nih'ao", "xi'anx'hao"] {
        let spans = spans_for_full_pinyin(raw, &trie());
        let flat_len = raw.replace('\'', "").len();
        let covered = spans.last().map_or(0, |s| s.fp_end);
        assert_eq!(covered, flat_len, "{raw}: span 应覆盖整个 flat 串");
    }
}
