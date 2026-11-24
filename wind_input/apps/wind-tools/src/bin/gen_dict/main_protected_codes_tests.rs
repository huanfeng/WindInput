//! 上游编码约定保护（[`apply_protected_codes`]）。
//!
//! 锁两件事：① 受保护码完整保留上游给出的候选次序；② 上游**没给出**次序时
//! （无权重列与显式最低档在解析后同形）由词频裁决，而不是让文件序随便定一个首选。
use super::*;

fn cfg() -> Config {
    Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    }
}

/// 按最终权重降序取文本，即用户看到的候选顺序。
fn texts_in_order(entries: &[Entry]) -> Vec<&str> {
    let mut v: Vec<&Entry> = entries.iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.weight));
    v.iter().map(|e| e.text.as_str()).collect()
}

/// ★ 上游原序必须原样保留：`cccc` 上游是 又40 > 双双30 > 叕20 > 敠10。
/// 补权叠加简码降权曾把首选变成「双双」（又 3010 → 双双 1319 - 1 = 1318）。
#[test]
fn upstream_order_is_preserved_verbatim() {
    let mut entries = vec![
        Entry::new("又".into(), "cccc".into(), 40, 0),
        Entry::new("双双".into(), "cccc".into(), 30, 1),
        Entry::new("叕".into(), "cccc".into(), 20, 2),
        Entry::new("敠".into(), "cccc".into(), 10, 3),
    ];
    let n = apply_protected_codes(&mut entries, &weight::Unigram::new(), &cfg());
    assert_eq!(n, 4);
    assert_eq!(texts_in_order(&entries), ["又", "双双", "叕", "敠"]);
}

/// ★ 上游本就不把键名汉字放首位的码（kkkk/uuuu/wwww/xxxx）同样照抄不动——
/// 保护的是「上游的安排」，不是「键名汉字必须第一」这条我们自己的规则。
#[test]
fn upstream_non_keyname_first_is_also_preserved() {
    let mut entries = vec![
        Entry::new("众人".into(), "wwww".into(), 40, 0),
        Entry::new("偷偷".into(), "wwww".into(), 30, 1),
        Entry::new("俗人".into(), "wwww".into(), 20, 2),
        Entry::new("人".into(), "wwww".into(), 10, 3),
    ];
    apply_protected_codes(&mut entries, &weight::Unigram::new(), &cfg());
    assert_eq!(texts_in_order(&entries), ["众人", "偷偷", "俗人", "人"]);
}

/// ★★ 回归：上游「无权重列」与「显式最低档 10」在解析后同形（`parse.rs` 空列返回 10）。
/// `qqqq` 的「狗狗」无权重列、「金」显式 10，仅靠文件序会让「狗狗」占首选——
/// 而 `qqqq` 的键名汉字是「金」。词频在此充当并列裁决。
#[test]
fn ties_are_broken_by_frequency_not_file_order() {
    let mut unigram = weight::Unigram::new();
    unigram.insert("金".into(), 500_000);
    unigram.insert("狗狗".into(), 900);
    // 文件序里「狗狗」在前，与上游 yaml 一致
    let mut entries = vec![
        Entry::new("狗狗".into(), "qqqq".into(), 10, 0),
        Entry::new("金".into(), "qqqq".into(), 10, 1),
    ];
    apply_protected_codes(&mut entries, &unigram, &cfg());
    assert_eq!(
        texts_in_order(&entries),
        ["金", "狗狗"],
        "同为最低档时应由词频裁决，而不是文件序"
    );
}

/// 优先级不同则词频**不得**翻盘：上游的显式安排恒压过词频。
#[test]
fn explicit_priority_outranks_frequency() {
    let mut unigram = weight::Unigram::new();
    unigram.insert("双双".into(), 9_000_000); // 词频远高于「又」
    unigram.insert("又".into(), 1);
    let mut entries = vec![
        Entry::new("又".into(), "cccc".into(), 40, 0),
        Entry::new("双双".into(), "cccc".into(), 30, 1),
    ];
    apply_protected_codes(&mut entries, &unigram, &cfg());
    assert_eq!(
        texts_in_order(&entries),
        ["又", "双双"],
        "上游优先级是主键，词频只在并列时才介入"
    );
}

/// 未受保护的码一条都不该被碰。
#[test]
fn unprotected_codes_are_untouched() {
    let mut entries = vec![
        Entry::new("工作".into(), "aawt".into(), 30, 0),
        Entry::new("又".into(), "cccc".into(), 40, 1),
    ];
    let n = apply_protected_codes(&mut entries, &weight::Unigram::new(), &cfg());
    assert_eq!(n, 1, "只应处理 cccc 这一条");
    assert_eq!(entries[0].weight, 30, "aawt 权重不得变动");
}

/// 保护带整体高于扩展带、且不侵入简码带——这两条是分带设计的边界。
#[test]
fn protected_band_sits_between_extra_and_shortcode_bands() {
    let c = cfg();
    let mut entries = vec![Entry::new("串口".into(), "kkkk".into(), 60, 0)];
    apply_protected_codes(&mut entries, &weight::Unigram::new(), &c);
    let w = entries[0].weight;
    assert!(
        w > c.extra.weight_max,
        "须高于扩展带顶 {}",
        c.extra.weight_max
    );
    assert!(w > c.fallback.priority_10, "须高于主库生僻字保底档");
    assert!(
        w < c.shortcodes.level3_base_weight,
        "不得侵入简码带 {}",
        c.shortcodes.level3_base_weight
    );
}
