use super::*;
use crate::pinyin::syllable::SyllableTrie;

#[test]
fn jqxy_v_becomes_u() {
    for (input, want) in [
        ("xv", "xu"),
        ("jv", "ju"),
        ("qv", "qu"),
        ("yv", "yu"),
        ("jvn", "jun"),
        ("qvan", "quan"),
        ("xve", "xue"),
        ("yvan", "yuan"),
    ] {
        assert_eq!(normalize_u_umlaut(input), want, "输入 {input}");
    }
}

#[test]
fn nl_ue_becomes_ve() {
    assert_eq!(normalize_u_umlaut("nue"), "nve");
    assert_eq!(normalize_u_umlaut("lue"), "lve");
    assert_eq!(normalize_u_umlaut("dalue"), "dalve", "词中同样生效");
}

/// n/l 后的 `v` 已经是音节表形态，不得再被动。
#[test]
fn nl_v_is_left_alone() {
    for s in ["nv", "lv", "nve", "lve", "nvhai", "lvyou"] {
        assert!(
            matches!(normalize_u_umlaut(s), Cow::Borrowed(_)),
            "{s} 不该被替换，且应零分配"
        );
    }
}

/// jqxy 之外的 `ue`、n/l 之外的 `u` 都不是 ü，不得误伤。
#[test]
fn unrelated_spellings_untouched() {
    for s in [
        "hao", "guo", "sui", "jue", "xue", "yue", // jqxy 后的 ue 本就是正字法形态
        "duo", "tuan", "shuo", "wu", "hue", // h 不在 nl 集合里
        "gue", "kue",
    ] {
        assert!(
            matches!(normalize_u_umlaut(s), Cow::Borrowed(_)),
            "{s} 不该被替换"
        );
    }
}

/// 首字符是 `v`/`u` 时没有前驱，两条规则都不成立（`v` 不能起音节）。
#[test]
fn leading_byte_has_no_predecessor() {
    assert!(matches!(normalize_u_umlaut("v"), Cow::Borrowed(_)));
    assert!(matches!(normalize_u_umlaut("ue"), Cow::Borrowed(_)));
    assert!(matches!(normalize_u_umlaut(""), Cow::Borrowed(_)));
}

/// 一串里多处触发须全部替换，且两条规则可以同时出现。
#[test]
fn multiple_occurrences_all_replaced() {
    assert_eq!(normalize_u_umlaut("xvxv"), "xuxu");
    assert_eq!(normalize_u_umlaut("nuexv"), "nvexu");
}

/// ★ 归一化的产物必须落在标准音节表里——否则这层白做。
#[test]
fn normalized_results_are_real_syllables() {
    let trie = SyllableTrie::new();
    for raw in ["xv", "jv", "qv", "yv", "jvn", "qvan", "xve", "nue", "lue"] {
        let norm = normalize_u_umlaut(raw);
        assert!(!trie.is_syllable(raw), "{raw} 归一化前本就不是音节");
        assert!(trie.is_syllable(&norm), "{raw} → {norm} 应是合法音节");
    }
}

/// 等长不变量：位置映射依赖它，破坏了会让 preedit 与 consumed_length 整体错位。
#[test]
fn replacement_preserves_length() {
    for s in ["xv", "nue", "nuexvlue", "wonuexvhao"] {
        assert_eq!(normalize_u_umlaut(s).len(), s.len(), "{s} 长度须不变");
    }
}
