use super::*;

/// 出厂默认必须是「不动用户的数据」。改这一条等于改产品语义，不是调参。
#[test]
fn default_is_keep() {
    assert_eq!(NewlineStyle::default(), NewlineStyle::Keep);
}

/// `keep` 一个字符都不许改——混合行尾也原样留着。
#[test]
fn keep_passes_everything_through() {
    let cr = char::from_u32(13).unwrap();
    let lf = char::from_u32(10).unwrap();
    let src = format!("a{cr}{lf}b{lf}c{cr}d");
    assert_eq!(NewlineStyle::Keep.apply(&src), src);
}

/// 三种行尾都要认，且 **CRLF 算一个换行**：逐字符转会把一个换行变成两个。
#[test]
fn every_eol_form_folds_into_one() {
    let cr = char::from_u32(13).unwrap();
    let lf = char::from_u32(10).unwrap();
    let src = format!("a{cr}{lf}b{lf}c{cr}d");

    assert_eq!(
        NewlineStyle::Cr.apply(&src),
        format!("a{cr}b{cr}c{cr}d"),
        "三处换行都该折成单个 CR"
    );
    assert_eq!(NewlineStyle::Lf.apply(&src), format!("a{lf}b{lf}c{lf}d"));
    assert_eq!(
        NewlineStyle::Crlf.apply(&src),
        format!("a{cr}{lf}b{cr}{lf}c{cr}{lf}d"),
        "已经是 CRLF 的那处不能翻倍"
    );
}

/// 连续换行是**多个**换行，不许合并——空行是用户的内容。
#[test]
fn consecutive_newlines_are_not_merged() {
    let cr = char::from_u32(13).unwrap();
    let lf = char::from_u32(10).unwrap();
    let src = format!("a{lf}{lf}{lf}b");
    assert_eq!(NewlineStyle::Cr.apply(&src), format!("a{cr}{cr}{cr}b"));
    assert_eq!(
        NewlineStyle::Crlf.apply(&format!("a{cr}{lf}{cr}{lf}b")),
        format!("a{cr}{lf}{cr}{lf}b"),
        "两个 CRLF 是两个换行，不是一个"
    );
}

/// 首尾换行不能丢，也不该产出多余内容。
#[test]
fn leading_and_trailing_newlines_survive() {
    let cr = char::from_u32(13).unwrap();
    let lf = char::from_u32(10).unwrap();
    assert_eq!(
        NewlineStyle::Cr.apply(&format!("{lf}a{lf}")),
        format!("{cr}a{cr}")
    );
}

/// 上屏文本绝大多数不含换行（逐字上屏），这条路必须零拷贝。
#[test]
fn text_without_newline_is_borrowed() {
    for style in [
        NewlineStyle::Keep,
        NewlineStyle::Cr,
        NewlineStyle::Lf,
        NewlineStyle::Crlf,
    ] {
        assert!(
            matches!(style.apply("你好世界"), Cow::Borrowed(_)),
            "{style:?}: 无换行的文本不该产生分配"
        );
    }
}

/// `keep` 即使文本里有换行也不拷贝。
#[test]
fn keep_never_allocates() {
    let lf = char::from_u32(10).unwrap();
    let src = format!("a{lf}b");
    assert!(matches!(NewlineStyle::Keep.apply(&src), Cow::Borrowed(_)));
}

/// 多字节字符紧贴换行时切片边界必须落在字符边界上（否则 panic）。
#[test]
fn multibyte_around_newline() {
    let lf = char::from_u32(10).unwrap();
    let cr = char::from_u32(13).unwrap();
    let src = format!("中{lf}文");
    assert_eq!(NewlineStyle::Cr.apply(&src), format!("中{cr}文"));
}

/// 配置串往返：认得的值原样回来，认不得的退化为「没配」而不是某个默认档。
#[test]
fn config_string_roundtrip() {
    for style in [
        NewlineStyle::Keep,
        NewlineStyle::Cr,
        NewlineStyle::Lf,
        NewlineStyle::Crlf,
    ] {
        assert_eq!(NewlineStyle::from_config(style.as_config()), Some(style));
    }
    assert_eq!(
        NewlineStyle::from_config("  CRLF "),
        Some(NewlineStyle::Crlf)
    );
    assert_eq!(
        NewlineStyle::from_config("rn"),
        None,
        "拼错的值必须是 None（跟随全局），不能悄悄固化成显式覆盖"
    );
}
