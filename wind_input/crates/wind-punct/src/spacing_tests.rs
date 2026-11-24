use super::*;

const ALL: SpacingRules = SpacingRules {
    cjk_latin: true,
    cjk_digit: true,
};

#[test]
fn classify_basic_scripts() {
    assert_eq!(classify('中'), CharClass::Cjk);
    assert_eq!(classify('か'), CharClass::Cjk);
    assert_eq!(classify('ㄅ'), CharClass::Cjk);
    assert_eq!(classify('\u{20000}'), CharClass::Cjk);
    assert_eq!(classify('a'), CharClass::Latin);
    assert_eq!(classify('É'), CharClass::Latin);
    assert_eq!(classify('ß'), CharClass::Latin);
    assert_eq!(classify('×'), CharClass::Other);
    assert_eq!(classify('7'), CharClass::Digit);
    assert_eq!(classify('ｈ'), CharClass::WideLatin);
    assert_eq!(classify('３'), CharClass::WideLatin);
    assert_eq!(classify('，'), CharClass::Other);
    assert_eq!(classify('・'), CharClass::Other);
    assert_eq!(classify(' '), CharClass::Other);
    assert_eq!(classify('한'), CharClass::Other);
}

#[test]
fn classify_unit_follows_client_representatives() {
    assert_eq!(classify_unit(0), CharClass::Other, "不可知");
    assert_eq!(classify_unit('a' as u16), CharClass::Latin);
    assert_eq!(classify_unit('?' as u16), CharClass::Other);
    assert_eq!(classify_unit(' ' as u16), CharClass::Other);
    assert_eq!(classify_unit('5' as u16), CharClass::Digit);
    assert_eq!(classify_unit(0x597D), CharClass::Cjk);
    assert_eq!(classify_unit(0xD840), CharClass::Cjk, "U+20000 的高代理");
    assert_eq!(classify_unit(0xD8BF), CharClass::Cjk, "U+3FFFF 的高代理");
    assert_eq!(classify_unit(0xD83D), CharClass::Other, "emoji 的高代理");
    assert_eq!(classify_unit(0xDC00), CharClass::Other, "孤立低代理");
}

#[test]
fn cjk_latin_boundaries() {
    use CharClass::*;
    let r = SpacingRules {
        cjk_latin: true,
        ..Default::default()
    };
    assert!(needs_space(Cjk, Latin, r));
    assert!(needs_space(Latin, Cjk, r));
    assert!(!needs_space(Cjk, Digit, r), "数字另有开关");
    assert!(
        !needs_space(Latin, Latin, r),
        "连续英文不管（词间空格由英文词上屏时自己补）"
    );
    assert!(!needs_space(Cjk, WideLatin, r), "全角字母不参与中英交界");
    assert!(!needs_space(Other, Latin, r));
    assert!(!needs_space(Latin, Other, r), "标点永不前补");
    assert!(!needs_space(Cjk, Cjk, r));
}

#[test]
fn cjk_digit_boundaries() {
    use CharClass::*;
    let r = SpacingRules {
        cjk_digit: true,
        ..Default::default()
    };
    assert!(needs_space(Cjk, Digit, r));
    assert!(needs_space(Digit, Cjk, r));
    assert!(!needs_space(Cjk, Latin, r));
}

#[test]
fn consecutive_latin_is_left_alone() {
    use CharClass::*;
    assert!(!needs_space(Latin, Latin, ALL));
    assert!(!needs_space(Digit, Latin, ALL), "3kg 不拆");
    assert!(!needs_space(WideLatin, WideLatin, ALL));
    assert!(!needs_space(Cjk, WideLatin, ALL), "全角字母不参与中英交界");
}

#[test]
fn nothing_when_all_off() {
    use CharClass::*;
    for p in [Cjk, Latin, Digit, WideLatin, Other] {
        for n in [Cjk, Latin, Digit, WideLatin, Other] {
            assert!(!needs_space(p, n, SpacingRules::default()));
            if p == Other || n == Other {
                assert!(!needs_space(p, n, ALL), "{p:?}|{n:?}");
            }
        }
    }
}
