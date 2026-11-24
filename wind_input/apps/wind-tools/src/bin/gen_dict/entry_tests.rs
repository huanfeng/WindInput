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
