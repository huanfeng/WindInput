//! 词条模型、Unicode 归类与过滤规则。
//!
//! Unicode 区间逐个对应 WindInput-Go 的 `tools/dictgen/enrich.go` 里的 `unicode.RangeTable`。
//! 这些区间决定哪些词条被丢弃、以及扩展库怎么分桶——收窄任何一段都会让原本入库的词条消失。

use crate::config::Config;

/// 一条词典条目。
///
/// `weight` 一字两用，与 Go 版的 `Entry.OrigWeight` 同构：解析阶段存 jidian 的原始
/// 优先级（10/20/30），赋权阶段被最终权重覆盖。保留这个重载是为了让 `fallback_weight`
/// 能读到原始优先级——**赋权时必须先读后写**，读到已覆盖的值会把生僻字全打成同一档。
#[derive(Debug, Clone)]
pub struct Entry {
    pub text: String,
    pub code: String,
    pub weight: i64,
    /// 0 = 普通词条；1/2/3 = 简码级别（单字且码长 ≤ 3）
    pub shortcode_level: usize,
    /// 在 jidian 中的原始行序，用于简码组内定序
    pub orig_pos: usize,
}

impl Entry {
    pub fn new(text: String, code: String, weight: i64, orig_pos: usize) -> Self {
        Self {
            text,
            code,
            weight,
            shortcode_level: 0,
            orig_pos,
        }
    }

    /// 词条是否为单字（按 Unicode 字符计，非字节）
    pub fn is_single_char(&self) -> bool {
        self.text.chars().take(2).count() == 1
    }
}

// ── Unicode 归类 ──────────────────────────────────────

const EMOJI_RANGES: &[(u32, u32)] = &[
    (0x2300, 0x23FF),
    (0x2600, 0x27BF),
    (0xFE00, 0xFE0F),
    (0x1F000, 0x1F02F),
    (0x1F0A0, 0x1F0FF),
    (0x1F300, 0x1F9FF),
    (0x1FA00, 0x1FAFF),
];

const PUA_RANGES: &[(u32, u32)] = &[(0xE000, 0xF8FF), (0xF0000, 0xFFFFF), (0x100000, 0x10FFFF)];

/// 表意文字码位区间（`require_cjk` / `classify` 用）。
///
/// 末项是**整个平面 2（SIP）与平面 3（TIP）**：这两个平面专用于表意文字，扩展 B–J 与
/// 兼容汉字补充全在其中，将来的扩展 K/L 亦然。原先逐块列举只到 `0x2CEAF`（扩展 E 末尾），
/// 于是 `require_cjk` 开启时会把扩展 F 及以后的字**整批丢弃**、`classify` 也分错桶。
/// 逐块列举的写法保证每升一版 Unicode 就静默漏一次，故按平面兜底。
const CJK_RANGES: &[(u32, u32)] = &[
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xF900, 0xFAFF),
    (0x20000, 0x3FFFF),
];

fn in_ranges(c: char, ranges: &[(u32, u32)]) -> bool {
    let v = c as u32;
    ranges.iter().any(|&(lo, hi)| v >= lo && v <= hi)
}

pub fn has_emoji(s: &str) -> bool {
    s.chars().any(|c| in_ranges(c, EMOJI_RANGES))
}

pub fn has_pua(s: &str) -> bool {
    s.chars().any(|c| in_ranges(c, PUA_RANGES))
}

pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| in_ranges(c, CJK_RANGES))
}

/// 是否全部由可打印 ASCII 组成（空串不算）
pub fn is_pure_latin(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| ('\u{20}'..='\u{7E}').contains(&c))
}

/// 五笔编码合法性：只认 a-y。
///
/// z 被排除是因为它在极点词库里是反查占位码，不是真实五笔码。
pub fn is_valid_code(code: &str) -> bool {
    code.chars().all(|c| ('a'..='y').contains(&c))
}

// ── 过滤 ──────────────────────────────────────────────

/// 判定词条去留；被丢弃时一并返回原因标记（写进 .filtered.tsv）。
pub fn should_keep(e: &Entry, cfg: &Config) -> Result<(), &'static str> {
    let f = &cfg.filter;
    if f.drop_z_code && e.code.starts_with('z') {
        return Err("z_code");
    }
    if f.drop_dollar && e.text.starts_with('$') {
        return Err("dollar_prefix");
    }
    if f.max_code_len > 0 && e.code.len() > f.max_code_len {
        return Err("code_too_long");
    }
    if !is_valid_code(&e.code) {
        return Err("code_invalid_chars");
    }
    if f.max_text_len > 0 && e.text.chars().count() > f.max_text_len {
        return Err("text_too_long");
    }
    if f.drop_emoji && has_emoji(&e.text) {
        return Err("emoji");
    }
    if f.drop_pua && has_pua(&e.text) {
        return Err("pua");
    }
    if f.drop_pure_latin && is_pure_latin(&e.text) {
        return Err("pure_latin");
    }
    if f.require_cjk && !has_cjk(&e.text) {
        return Err("no_cjk");
    }
    for rule in &cfg.drop_rules {
        let reason: &'static str = if rule.reason.is_empty() {
            "manual_rule"
        } else {
            // 原因串来自配置，需要 'static 生命周期写进报告；泄漏一次可接受
            // （规则条数是个位数，且进程随即退出）
            Box::leak(rule.reason.clone().into_boxed_str())
        };
        let hit = if !rule.code_prefix.is_empty() {
            e.code.starts_with(&rule.code_prefix)
        } else if !rule.code.is_empty() {
            e.code == rule.code
        } else {
            false
        };
        if hit && !rule.except_codes.iter().any(|c| c == &e.code) {
            return Err(reason);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DropRule;

    fn cfg() -> Config {
        Config {
            jidian_path: "a".into(),
            unigram_path: "b".into(),
            output_path: "c".into(),
            ..Default::default()
        }
    }

    fn entry(text: &str, code: &str) -> Entry {
        Entry::new(text.into(), code.into(), 10, 0)
    }

    #[test]
    fn code_validity_excludes_z() {
        assert!(is_valid_code("abcy"));
        assert!(!is_valid_code("abz"), "z 是反查占位码，不是五笔码");
        assert!(!is_valid_code("ab1"));
        assert!(is_valid_code(""), "空码交由上游 text/code 非空检查拦截");
    }

    #[test]
    fn pure_latin_boundaries() {
        assert!(is_pure_latin("abc"));
        assert!(is_pure_latin(" ~"), "0x20 与 0x7E 都在界内");
        assert!(!is_pure_latin("中"));
        assert!(!is_pure_latin(""), "空串不算纯 ASCII");
        assert!(!is_pure_latin("a\u{7F}"), "DEL 在界外");
    }

    #[test]
    fn cjk_covers_ext_b_beyond_bmp() {
        assert!(has_cjk("中"));
        assert!(has_cjk("\u{20000}"), "扩展 B 区需命中 R32 区间");
        assert!(!has_cjk("abc"));
    }

    #[test]
    fn emoji_detection_covers_supplementary_planes() {
        assert!(has_emoji("😂"));
        assert!(has_emoji("⌚"), "0x231A 落在 0x2300-0x23FF");
        assert!(!has_emoji("中"));
    }

    #[test]
    fn text_len_counts_chars_not_bytes() {
        // 16 个汉字 = 48 字节；按字节算会被误杀
        let e = entry(&"中".repeat(16), "abcd");
        assert!(should_keep(&e, &cfg()).is_ok());
        let e17 = entry(&"中".repeat(17), "abcd");
        assert_eq!(should_keep(&e17, &cfg()), Err("text_too_long"));
    }

    #[test]
    fn drop_rule_honors_except_codes() {
        let mut c = cfg();
        c.drop_rules = vec![DropRule {
            code_prefix: "co".into(),
            reason: "co_shortcut".into(),
            except_codes: vec!["cogw".into()],
            ..Default::default()
        }];
        assert!(
            should_keep(&entry("某", "cozz"), &c).is_err(),
            "co 前缀应被丢弃"
        );
        assert!(
            should_keep(&entry("驜", "cogw"), &c).is_ok(),
            "例外码应保留"
        );
        assert!(should_keep(&entry("中", "abcd"), &c).is_ok());
    }

    #[test]
    fn filter_order_matches_go_precedence() {
        // z 码 + 超长文本同时命中时，Go 先判 z_code；报告里的原因分布依赖这个顺序
        let e = entry(&"中".repeat(20), "zabc");
        assert_eq!(should_keep(&e, &cfg()), Err("z_code"));
    }
}
