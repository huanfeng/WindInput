use super::*;
use crate::CandidateSource;

fn cand(text: &str, is_common: bool) -> Candidate {
    Candidate {
        text: text.into(),
        source: CandidateSource::Pinyin,
        is_common,
        ..Default::default()
    }
}

#[test]
fn new_is_empty() {
    let store = CandidateStore::new();
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn set_candidates_shows_all() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![cand("a", true), cand("b", false)]);
    assert_eq!(store.len(), 2, "新数据到来即全量显示");
}

#[test]
fn set_filter_builds_view() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![cand("王", true), cand("尪", false), cand("往", true)]);
    store.set_filter(|c| c.is_common);
    assert_eq!(store.len(), 2);
    let texts: Vec<_> = store.displayed().iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["王", "往"]);
}

/// 快照不受筛选影响：清除筛选能原样拿回全量，且顺序不变。
/// （辅助码的退格还原与退出还原全靠这一条。）
#[test]
fn clear_filter_restores_full() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![cand("a", true), cand("b", false)]);
    store.set_filter(|c| c.is_common);
    assert_eq!(store.len(), 1);
    store.clear_filter();
    let texts: Vec<_> = store.displayed().iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["a", "b"], "原始快照与原序都要拿得回来");
}

/// 新数据到来即丢弃旧筛选。
///
/// 判据取「新数据全都显示出来」而非查一个状态位：新装的 `c` 是 `is_common=false`，
/// 旧筛选若还挂着就会把它滤掉、len 为 0。状态位断言反而测不出真实显示面。
#[test]
fn set_candidates_clears_filter() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![cand("a", true), cand("b", false)]);
    store.set_filter(|c| c.is_common);
    store.set_candidates(vec![cand("c", false)]);
    assert_eq!(store.len(), 1, "旧筛选必须随新数据一起丢弃");
}

#[test]
fn displayed_preserves_original_order() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![
        cand("e", false),
        cand("a", true),
        cand("d", false),
        cand("b", true),
        cand("c", false),
    ]);
    store.set_filter(|c| c.is_common);
    let displayed: Vec<_> = store.displayed().iter().map(|c| c.text.as_str()).collect();
    assert_eq!(displayed, vec!["a", "b"]);
}

#[test]
fn reapply_filter_overwrites() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![cand("a", true), cand("b", false), cand("c", true)]);
    store.set_filter(|c| c.is_common);
    assert_eq!(store.len(), 2);
    // 重新筛选：只留「a」
    store.set_filter(|c| c.text == "a");
    assert_eq!(store.len(), 1);
    assert_eq!(store.displayed()[0].text, "a");
}

/// 反复筛选恒从**快照**重筛，不是在上一次的结果上再筛。
///
/// 辅助码的退格还原全靠这条：`m` → `mz` → 退回 `m` 时，第二次的 `m` 必须能把
/// 上一步被 `mz` 滤掉的候选拿回来。在已筛结果上叠筛的话它们再也回不来。
#[test]
fn filter_always_reruns_from_snapshot() {
    let mut store = CandidateStore::new();
    store.set_candidates(vec![cand("a", true), cand("b", false), cand("c", true)]);
    store.set_filter(|c| c.text == "a");
    assert_eq!(store.len(), 1);
    store.set_filter(|c| c.is_common);
    let texts: Vec<_> = store.displayed().iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["a", "c"], "被上一次筛掉的 c 必须能回来");
}
