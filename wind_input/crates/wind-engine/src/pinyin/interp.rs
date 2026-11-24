//! 音节在三个编码域里的位置对齐。
//!
//! 拼音输入涉及三个编码域（见 `docs/design/pinyin-code-domains.md` §2）：
//!
//! ```text
//!   raw （击键域）    用户实际敲进去的字节：双拼 siyr、带分隔符 xi'an、简拼 xan
//!   syl （音节序列）  ["si","yuan"] / ["xi","an"]
//!   flat（全拼扁平）  siyuan / xian        ← 存储主键
//! ```
//!
//! [`SylSpan`] 把一个音节在这三者里的位置绑在一起，供双拼、带 `'` 全拼、简拼各条路径
//! 共用 raw↔flat 的往返映射。

use super::dag::Dag;
use super::syllable::SyllableTrie;

/// 一个音节在 raw / flat 两域的字节区间，外加它的音节文本本身。
///
/// 区间均为 `[start, end)`。`raw` 与 `flat` 的差值来自那些只占 raw 不占 flat 的字节：
/// 分隔符 `'`、双拼里的击键（2 键 → 变长音节）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SylSpan {
    /// 音节的全拼文本（如 `hao`）。
    pub pinyin: String,
    /// 在原始输入（击键域）中的起始字节位。
    pub raw_start: usize,
    /// 在原始输入中的结束字节位（不含）。
    pub raw_end: usize,
    /// 在全拼扁平串中的起始字节位。
    pub fp_start: usize,
    /// 在全拼扁平串中的结束字节位（不含）。
    pub fp_end: usize,
    /// 用户本义音节：双拼转换器为本键对取了模糊对偶声母时（小鹤开 s↔sh，`sx` 里
    /// `s` + ia/ua 都不成音节、读成 `shua`），按布局不开模糊该读的字面拼写（`sua`）。
    /// 其余情形（与 `pinyin` 相同、错音串垫底本就是字面读音、全拼）为 `None`。
    ///
    /// 只在模糊音提示开着时由转换器填（[`super::shuangpin::ShuangpinConverter::set_fuzzy`]），
    /// 供引擎拿用户本义与词库读音对齐。
    pub literal: Option<String>,
}

/// 为「全拼（可含 `'` 手动分隔符）」输入构造音节 span 序列。
///
/// `'` 是硬边界，音节不得跨越，故按它分段后各段独立切分——与
/// `PinyinEngine::segment_with_separators` 同款，只是额外记录了两域偏移。
///
/// 段内切分未覆盖的尾部（残码，如 `anx` 只切出 `an`）在两域同步跳过，
/// 保证后续段的偏移仍对齐；否则 `xi'anx'y` 这类输入里第三段的位置会整体错位。
pub fn spans_for_full_pinyin(input: &str, trie: &SyllableTrie) -> Vec<SylSpan> {
    let mut out = Vec::new();
    let mut raw_pos = 0usize;
    let mut fp_pos = 0usize;
    for (i, seg) in input.split('\'').enumerate() {
        if i > 0 {
            raw_pos += 1; // 跨过分隔符本身：占 raw 不占 flat
        }
        let seg_raw_start = raw_pos;
        for syl in Dag::build(seg, trie).maximum_match() {
            let n = syl.len();
            out.push(SylSpan {
                pinyin: syl,
                raw_start: raw_pos,
                raw_end: raw_pos + n,
                fp_start: fp_pos,
                fp_end: fp_pos + n,
                literal: None,
            });
            raw_pos += n;
            fp_pos += n;
        }
        // 残码：两域同步推进到段末，不留错位。
        let covered = raw_pos - seg_raw_start;
        raw_pos = seg_raw_start + seg.len();
        fp_pos += seg.len() - covered;
    }
    out
}

/// flat 域的已消费字节数 → raw 域的对应字节数。
///
/// 取覆盖 `fp_consumed` 的那个音节的 `raw_end`：候选消费的是整数个音节，
/// 落在音节中间时算到该音节末尾。
///
/// `raw` 用于一条收尾规则：紧跟的分隔符归入已消费侧（连续 `''` 一并吸收）。
/// `xi'an` 选「西」消费 flat 2 字节，若只返回 2，协调器按原始缓冲切片后剩下 `'an`
/// ——预编辑区会以分隔符开头。返回 3 才对。
///
/// `fp_consumed` 超出全部 span 时从末个 span 起按字节续推。正常不会发生——
/// [`spans_for_full_pinyin`] 让残码也占 flat 位，故 span 的 flat 总长恒等于剥除 `'`
/// 后的串长；这一支纯属防御，避免哪天构造侧改了语义就在这里返回错值。
pub fn map_fp_to_raw(spans: &[SylSpan], fp_consumed: usize, raw: &str) -> usize {
    if fp_consumed == 0 {
        return 0;
    }
    let bytes = raw.as_bytes();
    // 吸收紧跟的手动边界，使已消费段带走其后的 `'`。
    let absorb = |mut i: usize| {
        while i < bytes.len() && bytes[i] == b'\'' {
            i += 1;
        }
        i
    };
    for s in spans {
        if s.fp_end >= fp_consumed {
            return absorb(s.raw_end);
        }
    }
    // 防御性续推：从最后一个音节末尾起，按「非分隔符字节」计数补齐差额。
    let (mut i, done) = spans.last().map_or((0, 0), |s| (s.raw_end, s.fp_end));
    let mut remain = fp_consumed.saturating_sub(done);
    while i < bytes.len() && remain > 0 {
        if bytes[i] != b'\'' {
            remain -= 1;
        }
        i += 1;
    }
    absorb(i)
}

#[cfg(test)]
mod tests {
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
}
