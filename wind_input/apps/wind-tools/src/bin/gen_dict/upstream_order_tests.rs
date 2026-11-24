use super::*;
use crate::config::UpstreamOrderConfig;

fn cfg() -> Config {
    Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        upstream_order: UpstreamOrderConfig {
            enabled: true,
            max_freq_ratio: 26,
        },
        ..Default::default()
    }
}

fn e(text: &str, code: &str, weight: i64, pos: usize) -> Entry {
    Entry::new(text.into(), code.into(), weight, pos)
}

/// 按最终权重降序取文本，即用户看到的候选顺序。
fn order(entries: &[Entry]) -> Vec<&str> {
    let mut v: Vec<&Entry> = entries.iter().collect();
    v.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.orig_pos.cmp(&b.orig_pos))
    });
    v.iter().map(|e| e.text.as_str()).collect()
}

fn uni(pairs: &[(&str, i64)]) -> Unigram {
    pairs.iter().map(|(t, f)| (t.to_string(), *f)).collect()
}

/// ★ 主场景，现场取自实际产物 `tgyn`：上游「重启 20 > 生词 10」被词频补权翻成
/// 「生词 1497 > 重启 1479」。倍数 1.1 远在护栏内，应回归上游序。
#[test]
fn restores_upstream_order_within_guardrail() {
    let upstream = vec![e("重启", "tgyn", 20, 0), e("生词", "tgyn", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![e("生词", "tgyn", 1497, 1), e("重启", "tgyn", 1479, 0)];
    let u = uni(&[("重启", 2476), ("生词", 2717)]);

    let s = reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!(s.reordered, 1);
    assert_eq!(order(&produced), ["重启", "生词"]);
}

/// ★★ 权重轴高度守恒：重排只重新分配组内已有的权重值，集合必须一字不变——
/// 这是「跨码可比性不受影响」的实现保证，也是本方案区别于「优先级分带」的地方。
#[test]
fn weight_multiset_is_preserved() {
    let upstream = vec![
        e("丙", "abcd", 30, 0),
        e("甲", "abcd", 20, 1),
        e("乙", "abcd", 10, 2),
    ];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![
        e("甲", "abcd", 900, 1),
        e("乙", "abcd", 850, 2),
        e("丙", "abcd", 700, 0),
    ];
    let u = uni(&[("甲", 900), ("乙", 850), ("丙", 700)]);

    reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    let mut ws: Vec<i64> = produced.iter().map(|e| e.weight).collect();
    ws.sort_unstable();
    assert_eq!(ws, vec![700, 850, 900], "权重值集合不得变动");
    assert_eq!(order(&produced), ["丙", "甲", "乙"], "次序回归上游");
}

/// 护栏 ①：上游首选是 unigram 未命中的生僻词（`SD卡` 顶掉 `顶上` 那一类），不回归。
#[test]
fn holds_when_upstream_top_is_unseen() {
    let upstream = vec![e("川崎", "ktmd", 30, 0), e("噢", "ktmd", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![e("噢", "ktmd", 1500, 1), e("川崎", "ktmd", 180, 0)];
    let u = uni(&[("噢", 4092)]); // 「川崎」未命中

    let s = reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!((s.examined, s.reordered, s.held_unseen), (1, 0, 1));
    assert_eq!(order(&produced), ["噢", "川崎"], "维持词频序");
}

/// 护栏 ②：倍数超限不回归。现场取自 `wgbb`「鸽子(1019) ← 例子(29650)」= 29.1 倍。
#[test]
fn holds_when_ratio_exceeds_threshold() {
    let upstream = vec![e("鸽子", "wgbb", 40, 0), e("例子", "wgbb", 30, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![e("例子", "wgbb", 1949, 1), e("鸽子", "wgbb", 1311, 0)];
    let u = uni(&[("鸽子", 1019), ("例子", 29650)]);

    let s = reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!((s.examined, s.reordered, s.held_ratio), (1, 0, 1));
    assert_eq!(order(&produced), ["例子", "鸽子"]);
}

/// 护栏边界就在阈值上：`sgsv` 25.3 倍回归、`ddkh` 26.2 倍拦下（用户以样本夹定 26）。
#[test]
fn threshold_boundary_matches_calibration_samples() {
    // 25.3 倍：352 × 26 = 9152 > 8903 ⇒ 放行
    let up1 = vec![e("梗概", "sgsv", 30, 0), e("要不要", "sgsv", 10, 1)];
    let mut p1 = vec![e("要不要", "sgsv", 1600, 1), e("梗概", "sgsv", 900, 0)];
    let u1 = uni(&[("梗概", 352), ("要不要", 8903)]);
    reapply_upstream_order(&mut p1, &Snapshot::capture(&up1), &u1, &cfg());
    assert_eq!(order(&p1), ["梗概", "要不要"], "25.3 倍应回归");

    // 26.2 倍：475 × 26 = 12350 < 12449 ⇒ 拦下
    let up2 = vec![e("大路", "ddkh", 30, 0), e("套路", "ddkh", 10, 1)];
    let mut p2 = vec![e("套路", "ddkh", 1700, 1), e("大路", "ddkh", 800, 0)];
    let u2 = uni(&[("大路", 475), ("套路", 12449)]);
    reapply_upstream_order(&mut p2, &Snapshot::capture(&up2), &u2, &cfg());
    assert_eq!(order(&p2), ["套路", "大路"], "26.2 倍应拦下");
}

/// 简码带整组不参与：其权重是档位（9000+）不是词频，重排会把档位打散。
#[test]
fn shortcode_band_is_excluded() {
    let upstream = vec![e("乙", "abc", 30, 0), e("甲", "abc", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![e("甲", "abc", 9000, 1), e("乙", "abc", 8999, 0)];
    produced[0].shortcode_level = 3;
    produced[1].shortcode_level = 3;
    let u = uni(&[("甲", 500), ("乙", 400)]);

    let s = reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!(s.examined, 0, "简码条目不该进入判定");
    assert_eq!(produced[0].weight, 9000);
}

/// 受保护码整组不参与：`apply_protected_codes` 已按上游优先级赋过权，
/// 再重排一次会用「当前权重值集合」覆盖掉保护带的等距档位。
#[test]
fn protected_codes_are_excluded() {
    let upstream = vec![e("双双", "cccc", 30, 0), e("又", "cccc", 40, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![e("又", "cccc", 8020, 1), e("双双", "cccc", 8010, 0)];
    let u = uni(&[("又", 1318), ("双双", 500)]);

    let s = reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!(s.examined, 0);
    assert_eq!(produced[0].weight, 8020, "保护带档位不得被覆盖");
}

/// 自定义新增词不在上游序里，权重原样保留——上游对它没有意见，
/// 不该被挤进一个由上游序决定的位置。
#[test]
fn custom_word_weight_is_untouched() {
    let upstream = vec![e("丙", "abcd", 30, 0), e("甲", "abcd", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![
        e("新词", "abcd", 5000, 9), // custom_words 注入，上游无此条
        e("甲", "abcd", 900, 1),
        e("丙", "abcd", 700, 0),
    ];
    let u = uni(&[("甲", 900), ("丙", 700), ("新词", 300)]);

    reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!(produced[0].weight, 5000, "新增词权重不得变动");
    assert_eq!(
        order(&produced),
        ["新词", "丙", "甲"],
        "上游条目之间回归上游序"
    );
}

/// 首选本就一致时不进入判定——组内后段的次序不值得为它翻动权重。
#[test]
fn untouched_when_top_already_matches() {
    let upstream = vec![
        e("甲", "abcd", 30, 0),
        e("乙", "abcd", 20, 1),
        e("丙", "abcd", 10, 2),
    ];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![
        e("甲", "abcd", 900, 0),
        e("丙", "abcd", 800, 2),
        e("乙", "abcd", 700, 1),
    ];
    let u = uni(&[("甲", 900), ("乙", 700), ("丙", 800)]);

    let s = reapply_upstream_order(&mut produced, &snap, &u, &cfg());
    assert_eq!(s.examined, 0);
    assert_eq!(produced[1].weight, 800, "权重一动不动");
}

/// 开关关闭时整条逻辑不执行。
#[test]
fn disabled_switch_is_a_no_op() {
    let upstream = vec![e("重启", "tgyn", 20, 0), e("生词", "tgyn", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let mut produced = vec![e("生词", "tgyn", 1497, 1), e("重启", "tgyn", 1479, 0)];
    let u = uni(&[("重启", 2476), ("生词", 2717)]);
    let c = Config {
        upstream_order: UpstreamOrderConfig {
            enabled: false,
            max_freq_ratio: 26,
        },
        ..cfg()
    };

    assert_eq!(
        reapply_upstream_order(&mut produced, &snap, &u, &c),
        Stats::default()
    );
    assert_eq!(order(&produced), ["生词", "重启"]);
}
