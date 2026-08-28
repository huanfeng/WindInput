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
        // ★ 以下是审查抓出来的反例，也是最初漏测的形态：**受管辖字符后面跟一个组合
        // 码位**，整体成为一个多码位簇。若「多码位簇无覆盖时整簇忽略」，闸门开着就判成
        // 常用，而按 char 走时那个生僻汉字该判非常用——同一份数据两条路径答案相反。
        "\u{9B31}\u{FE0F}",       // 鬱 + 变体选择符（鬱 不在基表 ⇒ 应判非常用）
        "\u{6211}\u{FE0F}",       // 我 + 变体选择符（我 在基表 ⇒ 应判常用）
        "\u{9B31}\u{0301}",       // 鬱 + 组合锐音符
        "\u{E831}\u{FE0F}",       // PUA + 变体选择符（PUA 受管辖）
        "\u{20000}\u{FE0F}",      // 扩展 B + 变体选择符
        "\u{9B31}\u{FE0F}的东西", // 混在长串里
        // ★ 覆盖表里是裸 `⚽`，候选文本带 FE0F：闸门关着按 char 切命中那条覆盖，
        // 开着按簇切查不到——`has_user_rare` 若不逐 char 回落，两侧就在这里分叉。
        "\u{26BD}\u{FE0F}",
        "\u{26BD}",
        "球赛\u{26BD}\u{FE0F}开始",
    ];

    let mut closed = CommonChars::from_base(base.clone());
    closed.set_overrides([
        ("鬱".to_string(), true),
        ("的".to_string(), false),
        // ★ 裸 `⚽`(U+26BD) 被降级——批量操作写进去的正是这个形态（扫描按 char 收，
        // U+FE0F 不在 2600–26FF 区间内收不到），而候选文本是带 FE0F 的 `⚽️`。
        // 少了这一条，`has_user_rare` 那条分叉两侧恒为 false，测不出来。
        ("\u{26BD}".to_string(), false),
    ]);

    let mut open = CommonChars::from_base(base);
    open.set_overrides([
        ("鬱".to_string(), true),
        ("的".to_string(), false),
        ("\u{26BD}".to_string(), false),
        // 唯一的差别：多一条**与探针无关**的多码位覆盖，闸门因此打开。
        ("\u{1F468}\u{200D}\u{1F469}".to_string(), false),
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

    // 而只在 open 里登记的那条 ZWJ 序列，本就该只在 open 生效。
    let family = "\u{1F468}\u{200D}\u{1F469}";
    assert!(closed.is_string_common(family), "没登记时照旧放行");
    assert!(!open.is_string_common(family), "登记后必须判非常用");
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
