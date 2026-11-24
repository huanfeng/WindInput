use super::*;

const ALL: &[&str] = &[
    USER_DICT,
    USER_DICT_LIB,
    USER_DICTS_META,
    SHADOW,
    TEMP,
    DRAFT,
    FREQ,
    ASSOC,
    PHRASES,
    COMPLETION,
    QUICK_FORMAT,
    COMMON_CHARS,
    CHARSETS,
    STATS,
    PHRASE_SYS_HASH,
];

/// 程序也在高频写、只经「编辑文件」入口改的文件（临时词 / 词频）的统一后缀。
const EDIT_VIA_SESSION: &str = "可在设置里用「编辑文件」修改，编辑期间新学到的内容会在完成后合并；文件头（--- 到 ...）请勿修改。";

#[test]
fn every_line_follows_the_format() {
    for s in ALL {
        assert!(s.starts_with("# 清风输入法 · "), "{s}");
        assert!(!s.contains('\n'), "{s}");
        assert!(
            [
                "可手动编辑；文件头（--- 到 ...）请勿修改。",
                "可手动编辑。",
                "由程序自动维护，请勿手动修改。",
                EDIT_VIA_SESSION,
            ]
            .iter()
            .any(|t| s.ends_with(&format!("。{t}"))),
            "{s}"
        );
    }
}

#[test]
fn editable_learned_lines_point_at_the_edit_entry() {
    assert!(TEMP.ends_with(EDIT_VIA_SESSION));
    assert!(FREQ.ends_with(EDIT_VIA_SESSION));
}

#[test]
fn table_kinds_map_to_their_lines() {
    for (k, want) in [
        ("words", USER_DICT),
        ("shadow", SHADOW),
        ("phrases", PHRASES),
        ("temp", TEMP),
        ("draft", DRAFT),
        ("freq", FREQ),
        ("assoc", ASSOC),
        ("completion", COMPLETION),
    ] {
        assert_eq!(for_kind(k), Some(want));
    }
    assert_eq!(for_kind("other"), None);
}
