use super::*;
use crate::pinyin::syllable::{STANDARD_SYLLABLES, SyllableTrie};

/// ★★ **穷尽性自证**：标准音节表里 ü 的落点只有两类，本模块的两条规则各覆盖一类。
///
/// 这条不是在测 `normalize_u_umlaut`，而是在锁**它的前提**——音节表若日后新增
/// 一个含 `v` 或 jqxy+u 的音节，规则的覆盖面就不再完整，而那不会有任何编译错误。
/// 断言写成「集合完全相等」而不是「包含」，正是为了让新增项立刻红。
#[test]
fn umlaut_landing_sites_in_syllable_table_are_fully_covered() {
    let with_v: Vec<&str> = STANDARD_SYLLABLES
        .iter()
        .copied()
        .filter(|s| s.contains('v'))
        .collect();
    assert_eq!(
        with_v,
        ["lv", "lve", "nv", "nve"],
        "含 `v` 的音节全集变了 ⇒ 归一化规则的覆盖面须重新推导"
    );

    let jqxy_u: Vec<&str> = STANDARD_SYLLABLES
        .iter()
        .copied()
        .filter(|s| {
            let b = s.as_bytes();
            b.len() >= 2 && is_jqxy(b[0]) && b[1] == b'u'
        })
        .collect();
    assert_eq!(
        jqxy_u,
        [
            "ju", "juan", "jue", "jun", "qu", "quan", "que", "qun", "xu", "xuan", "xue", "xun",
            "yu", "yuan", "yue", "yun"
        ],
        "jqxy+u 的音节全集变了 ⇒ 同上"
    );

    // 每一个 jqxy+u 音节，把 `u` 写成 `v` 后都必须能归一回来。
    for syl in &jqxy_u {
        let typed = format!("{}v{}", &syl[..1], &syl[2..]);
        assert_eq!(
            normalize_u_umlaut(&typed),
            *syl,
            "`{typed}` 应归一回 `{syl}`"
        );
    }
}

/// `ue` → `ve` 规则的**无损性**：n/l 后不存在以 `ue` 起头的合法音节，
/// 故这条规则不可能夺走任何原本有解的单音节切分。
#[test]
fn ue_rule_steals_no_valid_syllable() {
    let trie = SyllableTrie::new();
    for initial in ["n", "l"] {
        let victim = format!("{initial}ue");
        assert!(
            !trie.is_syllable(&victim),
            "`{victim}` 若成了合法音节，`ue`→`ve` 就变成有损改写，规则须重新论证"
        );
    }
    // 反向：归一化的目标必须真的存在，否则这条规则是空转。
    assert!(trie.is_syllable("nve") && trie.is_syllable("lve"));
}

/// 归一化对**任何**标准音节都是恒等的——音节表里的串本就是正字法形态。
/// 这条一旦红，说明规则误伤了合法拼写（比如错加「n/l 后 v→u」那一臂）。
#[test]
fn every_standard_syllable_is_a_fixed_point() {
    for syl in STANDARD_SYLLABLES {
        assert!(
            matches!(normalize_u_umlaut(syl), Cow::Borrowed(_)),
            "`{syl}` 是标准音节，归一化须恒等"
        );
    }
}
