use super::*;
use crate::CommonChars;

/// 装配之后的真实形态：常用汉字类（给默认判定）+ 被点名纳入生僻模式的类。
fn registry(base: &[char], admitted: &[&str]) -> crate::CharsetRegistry {
    let mut specs = vec![crate::ClassSpec {
        key: "common_han".into(),
        members: base.iter().map(|c| c.to_string()).collect(),
        scope: Some(crate::Scope::Han),
        default_common: Some(true),
        outside_common: Some(false),
        order: 50,
        ..Default::default()
    }];
    if !admitted.is_empty() {
        specs.push(crate::ClassSpec {
            key: "admitted".into(),
            members: admitted.iter().map(|s| s.to_string()).collect(),
            in_rare: true,
            order: 10,
            ..Default::default()
        });
    }
    let (reg, dropped) = crate::CharsetRegistry::compile(specs);
    assert!(dropped.is_empty());
    reg
}

/// 谁都没点名的 registry：常用汉字类照常在，但 `in_rare` 恒假。
///
/// ⚠️ **不能用 `CharsetRegistry::default()`**：`rare_admits` 的第三个参数身兼两职，
/// 空 registry 会让 `is_string_common` 对所有字符兜底判常用，于是「生僻汉字要进」
/// 那一半根本测不出来。
fn admitting_none() -> crate::CharsetRegistry {
    registry(&['我', '你', '好', '的'], &[])
}

/// 生僻字准入的基本盘：常用字出局、生僻字进来、多字词一律不进。
#[test]
fn rare_admits_only_single_uncommon_chars() {
    let cc = CommonChars::from_base(['我', '你', '好', '的']);
    let none = admitting_none();
    assert!(!rare_admits("我", &cc, &none), "常用字不进");
    assert!(rare_admits("龘", &cc, &none), "生僻汉字要进");
    // 「严格只出单字」（用户 2026-08-24 拍板）：多字词无论常不常用都不进。
    assert!(!rare_admits("你好", &cc, &none));
    assert!(!rare_admits("龘龘", &cc, &none));
    assert!(!rare_admits("", &cc, &none));
    assert!(!rare_admits(" ", &cc, &none), "空白不是字");
}

/// ★★★ **取反陷阱**：域外字符（emoji/注音/假名）不会因为「不常用」就自动进来。
///
/// `is_string_common` 对它们恒为 true（语义是「忽略」而非「常用」），所以把准入写成
/// `!is_string_common` 的话，这些字符会被**恒滤掉**，且代码里找不到任何一处写着要滤
/// 掉它们——这正是本判据必须是正向白名单的原因。两个方向都钉住：不配 `extra` 时不进，
/// 配了才进。
#[test]
fn out_of_scope_chars_need_explicit_admission() {
    let cc = CommonChars::from_base(['我']);
    let none = registry(&['我'], &[]);
    for s in ["😀", "ㄅ", "あ", "⿰"] {
        assert!(
            !rare_admits(s, &cc, &none),
            "{s} 未显式纳入时不该进生僻字模式"
        );
    }
    // 字表里只列基字符 `⚽`——`⚽️`（带 U+FE0F）靠逐 char 回落命中，见
    // `CharsetRegistry::cluster_hits`。
    let emoji = registry(&['我'], &["😀", "⚽"]);
    assert!(rare_admits("😀", &cc, &emoji), "点名纳入的就该进");
    assert!(rare_admits("⚽️", &cc, &emoji), "带变体选择符的同样要进");
    assert!(!rare_admits("ㄅ", &cc, &emoji), "没纳入的仍然不进");
    // 汉字不会因为纳入了 emoji 就被带进来（常用字始终出局）。
    assert!(!rare_admits("我", &cc, &emoji));
}

/// 用户手动设成生僻的字符**直接进**，与它是不是汉字无关。
///
/// 走的是 `is_string_common` 里「覆盖优先于作用域判断」那条既有路径，故这里不必、
/// 也不该为域外字符再写一遍准入——否则用户在词库管理页把 `、` 设成生僻，
/// 生僻字模式里却看不到它。
#[test]
fn user_marked_rare_chars_are_admitted() {
    let mut cc = CommonChars::from_base(['我', '好']);
    cc.set_overrides([("好".to_string(), false), ("、".to_string(), false)]);
    let none = registry(&['我', '好'], &[]);
    assert!(rare_admits("好", &cc, &none), "用户降级的汉字要进");
    assert!(
        rare_admits("、", &cc, &none),
        "用户降级的域外字符同样要进，无需纳入任何类"
    );
    assert!(!rare_admits("我", &cc, &none), "没表过态的常用字仍出局");
}

/// 「一个字」按字素簇算，不按码位数——`⚽️` 是 2 个码位、`👨‍👩‍👧` 是 5 个，都算一个字。
///
/// ⛔ 别退回「跳过修饰码位再数基础字符」那种自己列举 Unicode 规则的写法，
/// 本仓已经在 `single_markable_char` 那轮走死过一次。
#[test]
fn one_char_means_one_grapheme_cluster() {
    let cc = CommonChars::from_base(['我']);
    // 只列基字符/首码位：肤色、ZWJ、区域指示符的第二半都靠逐 char 回落命中。
    let emoji = registry(&['我'], &["⚽", "👍", "👨", "🇨", "😀"]);
    for s in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳"] {
        assert!(rare_admits(s, &cc, &emoji), "{s} 是一个字素簇，应算单字");
    }
    assert!(!rare_admits("😀😀", &cc, &emoji), "两个字素簇不算单字");
}
