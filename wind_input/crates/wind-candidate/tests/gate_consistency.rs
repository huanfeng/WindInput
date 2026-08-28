//! `has_multi_char_keys` 闸门两侧必须给出**同一个答案**。
//!
//! 闸门为假时按 `char` 切、为真时按字素簇切。两条路径若对同一份数据答案不同，症状是
//! 「登记了一个 emoji 序列之后，别的字的判定跟着变了」——极难联想到是那条 emoji 引起的。
use wind_candidate::CommonChars;

/// 逐字符逐串对照：加入一条多码位覆盖（打开闸门）后，**其余字符的判定一个都不能变**。
#[test]
fn opening_the_gate_does_not_change_any_other_verdict() {
    let base: Vec<char> = "我的东西输入法一二三".chars().collect();
    let probes = [
        "我",
        "的",
        "鬱",
        "东西",
        "输入法",
        "hello",
        "、",
        "，",
        "ㄅ",
        "⿰",
        "あ",
        "①",
        "A",
        "7",
        "新iPhone",
        "一二三",
        "中文abc混排",
        "\u{20000}",
        "\u{323B0}",
        "\u{E831}",
    ];

    let mut closed = CommonChars::from_base(base.clone());
    closed.set_overrides([("鬱".to_string(), true), ("的".to_string(), false)]);

    let mut open = CommonChars::from_base(base);
    open.set_overrides([
        ("鬱".to_string(), true),
        ("的".to_string(), false),
        // 唯一的差别：多一条多码位覆盖，闸门因此打开。
        ("\u{26BD}\u{FE0F}".to_string(), false),
    ]);

    for p in probes {
        assert_eq!(
            closed.is_string_common(p),
            open.is_string_common(p),
            "{p:?} 的判定不该因为「别处登记了一条 emoji」而改变"
        );
        assert_eq!(
            closed.has_user_rare(p),
            open.has_user_rare(p),
            "{p:?} 的 user_rare 判定同样不该变"
        );
    }

    // 而那条 emoji 本身，只在闸门开着的那份里生效。
    let ball = "\u{26BD}\u{FE0F}";
    assert!(closed.is_string_common(ball), "没登记时照旧放行");
    assert!(!open.is_string_common(ball), "登记后必须判非常用");
}

/// 闸门开着时，**单码位字符**的判定仍与逐 char 路径一致——包括混在长句里的。
#[test]
fn gate_open_still_matches_char_path_on_plain_text() {
    let base: Vec<char> = "常用字表".chars().collect();
    let mut cc = CommonChars::from_base(base);
    cc.set_overrides([
        ("字".to_string(), false),
        ("\u{1F468}\u{200D}\u{1F469}".to_string(), false),
    ]);
    // 「字」被降级 ⇒ 含它的串一律非常用；不含它的照旧。
    assert!(!cc.is_string_common("字"));
    assert!(!cc.is_string_common("常用字表"));
    assert!(cc.is_string_common("常用"));
    assert!(cc.is_string_common("表"));
    // 单字判与串判一致（闸门开着也不能分叉）。
    for ch in "常用字表".chars() {
        assert_eq!(
            cc.is_char_common(ch),
            cc.is_string_common(&ch.to_string()),
            "{ch} 按字判与按串判分叉了"
        );
    }
}
