use super::*;

/// kind 的取值口径：`parse` / `as_str` 互逆，缺省是候选窗（与不带 kind 的旧请求一致）。
#[test]
fn preview_kind_values_are_round_trippable_and_default_to_candidate() {
    assert_eq!(PreviewKind::default(), PreviewKind::Candidate);
    for k in [PreviewKind::Candidate, PreviewKind::Toolbar] {
        assert_eq!(PreviewKind::parse(k.as_str()), Some(k), "{}", k.as_str());
    }
    assert_eq!(
        PreviewKind::parse("candidate"),
        Some(PreviewKind::Candidate)
    );
    assert_eq!(PreviewKind::parse("toolbar"), Some(PreviewKind::Toolbar));
    assert_eq!(
        PreviewKind::parse("Candidate"),
        None,
        "大小写敏感，拼错必须报错而不是静默画候选窗"
    );
    assert_eq!(PreviewKind::parse(""), None);
    assert_eq!(PreviewKind::parse("cand"), None);
}

/// fit 折算：缺省 = 1；Contain 比取两维 min、只给一维按该维算；clamp 到 1..=3；
/// 自然尺寸非正与 fit ≤ 0 的异常值不放大（防撑爆，正常调用到不了这里）。
#[test]
fn preview_budget_rejects_oversized_and_caps_fit() {
    assert!(preview_size_ok(MAX_PREVIEW_EDGE_PX, 1));
    assert!(!preview_size_ok(MAX_PREVIEW_EDGE_PX + 1, 1), "单边超限");
    assert!(!preview_size_ok(8000, 8000), "单边没超但总像素超");
    assert!(!preview_size_ok(0, 10), "零尺寸");
    // 预算内的放大原样保留。
    assert_eq!(cap_fit_ratio(2.0, 100.0, 50.0), 2.0);
    // 压到单边上限。
    let r = cap_fit_ratio(3.0, 4000.0, 100.0);
    assert!((r * 4000.0 - MAX_PREVIEW_EDGE_PX as f32).abs() < 1.0);
    // 压到总像素上限。
    let r = cap_fit_ratio(3.0, 4000.0, 4000.0);
    assert!(((4000.0 * r) * (4000.0 * r)) as u64 <= MAX_PREVIEW_PIXELS + 4000 * 8);
    // r = 1 都装不下：不放大，交给 preview_size_ok 拒。
    assert_eq!(cap_fit_ratio(3.0, 9000.0, 10.0), 1.0);
}

#[test]
fn fit_ratio_clamps_contain_ratio() {
    let n = (240.0, 64.0);
    assert_eq!(preview_fit_ratio(None, None, n.0, n.1), 1.0, "缺省不放大");
    assert_eq!(
        preview_fit_ratio(Some(240), Some(64), n.0, n.1),
        1.0,
        "fit = 自然尺寸 → 倍率 1"
    );
    assert_eq!(
        preview_fit_ratio(Some(480), Some(128), n.0, n.1),
        2.0,
        "两维取 min"
    );
    assert_eq!(
        preview_fit_ratio(Some(960), Some(128), n.0, n.1),
        2.0,
        "窄的一维说了算"
    );
    assert_eq!(
        preview_fit_ratio(Some(480), None, n.0, n.1),
        2.0,
        "只给一维按该维算"
    );
    assert_eq!(
        preview_fit_ratio(Some(2400), Some(640), n.0, n.1),
        3.0,
        "封顶 3"
    );
    assert_eq!(
        preview_fit_ratio(Some(120), Some(64), n.0, n.1),
        1.0,
        "fit 小于自然 → 不缩小，仍 1:1"
    );
    assert_eq!(preview_fit_ratio(Some(480), None, 0.0, 64.0), 1.0);
    assert_eq!(preview_fit_ratio(Some(0), None, n.0, n.1), 1.0);
}

#[test]
fn sample_clamps_count_and_labels_fall_back_to_digits() {
    assert_eq!(preview_candidates(0, |_| String::new()).len(), 1);
    let all = preview_candidates(99, |i| theme_index_label(&["a".into(), String::new()], i));
    assert_eq!(all.len(), 10);
    let labels: Vec<_> = all.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["a", "2", "3", "4", "5", "6", "7", "8", "9", "0"]);
    assert_eq!(all[2].comment.as_str(), "wqiy");
    assert_eq!(preview_preedit("'"), "ni'hao");
    assert_eq!(preview_preedit(" "), "ni hao");
    assert_eq!(preview_preedit(""), "nihao");
}
