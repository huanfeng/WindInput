use super::CandidateScene;

/// 总额够用：人人原样满足，不做无谓截断。
#[test]
fn everyone_fits_when_total_is_enough() {
    let got = CandidateScene::water_fill(&[100.0, 200.0, 50.0], 1000.0, 0.0);
    assert_eq!(got, vec![100.0, 200.0, 50.0]);
}

/// 核心性质：短需求原样保留，把没用完的份额让给长需求，而不是一刀切均分。
///
/// 均分会给长的 600/3=200，water-filling 给到 400——这正是「1 条长整句 + 8 个短词」
/// 这个最常见场景里两者的观感差别。
#[test]
fn spare_share_flows_to_the_long_one() {
    let got = CandidateScene::water_fill(&[100.0, 100.0, 1000.0], 600.0, 0.0);
    assert_eq!(got, vec![100.0, 100.0, 400.0]);
    assert_eq!(got.iter().sum::<f32>(), 600.0);
}

/// 顺序无关：同一组需求换个排列，各自拿到的份额不变（内部按需求升序处理，结果按原序返回）。
#[test]
fn allocation_is_order_independent() {
    let a = CandidateScene::water_fill(&[1000.0, 100.0, 100.0], 600.0, 0.0);
    assert_eq!(a, vec![400.0, 100.0, 100.0]);
}

/// 不溢出保证：只要 total ≥ 0 且不触发 floor，`Σ result ≤ total` 与项数无关。
/// 这正是贪心（先到先得 + 下限）给不了的性质——那种做法下第一项吃光预算，
/// 后面每项仍各自加上「固定开销 + 文字下限」，累加必然突破上限。
#[test]
fn sum_never_exceeds_total_regardless_of_count() {
    for n in 1..=20usize {
        let demands: Vec<f32> = (0..n).map(|i| (i as f32 + 1.0) * 500.0).collect();
        let total = 1000.0;
        let got = CandidateScene::water_fill(&demands, total, 0.0);
        let sum: f32 = got.iter().sum();
        assert!(sum <= total + 0.01, "n={n} 时 Σ={sum} 超出 total={total}");
    }
}

/// 全员都超份额：平分总额。
#[test]
fn all_oversized_split_evenly() {
    let got = CandidateScene::water_fill(&[900.0, 900.0, 900.0], 300.0, 0.0);
    assert_eq!(got, vec![100.0, 100.0, 100.0]);
}

/// floor 是「无解时的保底」：固定开销已超预算（total ≤ 0）时每项仍拿到 floor，
/// 总和因此可能超出 total——这是刻意取舍，宁可溢出被裁，也不少显示候选
/// （选中键位与候选列表绑定，少一个会让用户按 8 选到错的词）。
#[test]
fn floor_guarantees_minimum_even_when_insolvent() {
    let got = CandidateScene::water_fill(&[900.0, 900.0], 0.0, 20.0);
    assert_eq!(got, vec![20.0, 20.0]);
    let got = CandidateScene::water_fill(&[900.0, 900.0], -500.0, 20.0);
    assert_eq!(got, vec![20.0, 20.0]);
}

/// 空输入不 panic（无候选时的退化情形）。
#[test]
fn empty_demands_yield_empty_result() {
    assert!(CandidateScene::water_fill(&[], 100.0, 0.0).is_empty());
}
