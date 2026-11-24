use super::*;
use crate::entry::Entry;

fn cfg() -> Config {
    Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    }
}

fn e(text: &str) -> Entry {
    Entry::new(text.into(), "abcd".into(), 10, 0)
}

#[test]
fn median_freq_maps_to_target_median() {
    // 归一化的定义点：中位频次的词应拿到 target_median 权重
    let c = cfg();
    let median = 1000.0_f64;
    let log_median = (median + 1.0).log10();
    assert_eq!(compute_weight(1000, log_median, &c), c.target_median);
}

#[test]
fn median_of_even_count_averages_middle_two() {
    let mut u = Unigram::new();
    u.insert("a".into(), 10);
    u.insert("b".into(), 20);
    u.insert("c".into(), 30);
    u.insert("d".into(), 40);
    let entries = vec![e("a"), e("b"), e("c"), e("d")];
    assert_eq!(median_raw_freq(&entries, &u), 25.0);
}

#[test]
fn median_ignores_entries_missing_from_unigram() {
    let mut u = Unigram::new();
    u.insert("a".into(), 100);
    let entries = vec![e("a"), e("不存在"), e("也不存在")];
    assert_eq!(
        median_raw_freq(&entries, &u),
        100.0,
        "未命中条目不该参与中位数"
    );
}

#[test]
fn empty_unigram_falls_back_without_dividing_by_zero() {
    assert_eq!(median_raw_freq(&[e("x")], &Unigram::new()), 1000.0);
    assert_eq!(compute_weight(500, 0.0, &cfg()), cfg().weight_min);
}

#[test]
fn weight_is_clamped_to_range() {
    let c = cfg();
    let tiny_log_median = 0.001; // 放大到超出上限
    assert_eq!(compute_weight(999999, tiny_log_median, &c), c.weight_max);
    assert_eq!(compute_weight(0, 3.0, &c), c.weight_min);
}

#[test]
fn fallback_tiers_follow_jidian_priority() {
    let c = cfg();
    assert_eq!(fallback_weight(30, &c), 180);
    assert_eq!(fallback_weight(25, &c), 150, "20..29 落 priority_20");
    assert_eq!(fallback_weight(20, &c), 150);
    assert_eq!(fallback_weight(10, &c), 120);
    assert_eq!(fallback_weight(0, &c), 120, "无优先级按最低档");
}

#[test]
fn higher_freq_never_yields_lower_weight() {
    let c = cfg();
    let lm = 3.0;
    let mut prev = 0;
    for f in [1, 10, 100, 1000, 10_000, 100_000] {
        let w = compute_weight(f, lm, &c);
        assert!(
            w >= prev,
            "权重须随频次单调不减: freq={f} w={w} prev={prev}"
        );
        prev = w;
    }
}
