use super::*;

fn cfg() -> Config {
    Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    }
}

/// `[demotion]` 已退役、**默认关**（取代者是运行时的出简让全，见 config.rs）。
///
/// 下面几条降权用例测的是函数自身的逻辑，留着是为了「将来若重新开启，逻辑不至于
/// 已经腐烂」，故须显式打开。
///
/// ⚠️ 不显式打开的话，`apply_demotion` 在入口就 `return` 空集，于是三条
/// `assert!(...is_empty())` 恒真——**测试全绿，实际什么都没测**。
fn cfg_demotion_on() -> Config {
    let mut c = cfg();
    c.demotion.enabled = true;
    c
}

fn e(text: &str, code: &str, weight: i64, pos: usize) -> Entry {
    Entry::new(text.into(), code.into(), weight, pos)
}

#[test]
fn levels_follow_code_length_for_single_chars_only() {
    let mut v = vec![
        e("一", "g", 10, 0),
        e("地", "fb", 10, 1),
        e("在", "dhf", 10, 2),
        e("中国", "khl", 10, 3), // 词组：不是简码
        e("感", "dgkn", 10, 4),  // 4 码：不是简码
    ];
    assign_shortcode_weights(&mut v, &cfg());
    assert_eq!(v[0].shortcode_level, 1);
    assert_eq!(v[1].shortcode_level, 2);
    assert_eq!(v[2].shortcode_level, 3);
    assert_eq!(v[3].shortcode_level, 0, "词组不算简码");
    assert_eq!(v[4].shortcode_level, 0, "4 码不算简码");
}

#[test]
fn same_code_group_descends_by_original_order() {
    let c = cfg();
    // 同为 2 简码 "fb"，文件序 5 在前、2 在后
    let mut v = vec![e("乙", "fb", 10, 5), e("甲", "fb", 10, 2)];
    assign_shortcode_weights(&mut v, &c);
    let base = c.shortcodes.level2_base_weight;
    assert_eq!(v[1].weight, base, "orig_pos 小的排首位");
    assert_eq!(v[0].weight, base - 1);
}

#[test]
fn shortcode_band_stays_above_regular_ceiling() {
    let c = cfg();
    let mut v = vec![e("在", "dhf", 10, 0)];
    assign_shortcode_weights(&mut v, &c);
    assert!(
        v[0].weight > c.regular_max(),
        "简码权重须严格高于普通词条上限"
    );
}

/// **默认必须是关的**——本条钉的是 `DemotionConfig::default()`，不是 gen_dict.toml。
///
/// 配置结构体带 `#[serde(default)]`，缺 `[demotion]` 段的配置文件静默取默认值。
/// 若默认留 true，词库层会再让位一次，与运行时的出简让全叠加：字被压到词后面两遍。
/// 而产物里看不出异常——权重都是合法值，只有实际打字才发现首选不对。
#[test]
fn demotion_is_disabled_by_default() {
    let c = cfg(); // 刻意不用 cfg_demotion_on()
    let mut v = vec![
        e("中", "k", 9999, 0),
        e("中", "khkg", 5000, 1),
        e("口中", "khkg", 4500, 2), // 这组入参在开启时必定触发降权
    ];
    v[0].shortcode_level = 1;
    assert!(
        apply_demotion(&mut v, &c).is_empty(),
        "[demotion] 已退役，默认必须关"
    );
    assert_eq!(v[1].weight, 5000, "关闭时权重原样不动");
}

#[test]
fn demotion_yields_to_strong_second_candidate() {
    let c = cfg_demotion_on();
    let mut v = vec![
        e("中", "k", 9999, 0),      // 一简
        e("中", "khkg", 5000, 1),   // 同字占 4 码首选
        e("口中", "khkg", 4500, 2), // 第二候选：词组，权重 ≥ 800
    ];
    v[0].shortcode_level = 1;
    let demoted = apply_demotion(&mut v, &c);
    assert_eq!(v[1].weight, 4499, "应降到第二候选之下");
    assert_eq!(v[0].weight, 9999, "简码条目本身不动");
    // 返回的是被降者本身的 (code, text)，不是让位得来的第二候选
    assert_eq!(
        demoted.into_iter().collect::<Vec<_>>(),
        vec![("khkg".to_string(), "中".to_string())]
    );
}

#[test]
fn demotion_skipped_when_gap_too_large() {
    let c = cfg_demotion_on();
    // gap = (5000-900)/5000 = 0.82 > 0.65 → 首字优势太大，保留
    let mut v = vec![
        e("中", "k", 9999, 0),
        e("中", "khkg", 5000, 1),
        e("口中", "khkg", 900, 2),
    ];
    v[0].shortcode_level = 1;
    assert!(apply_demotion(&mut v, &c).is_empty());
    assert_eq!(v[1].weight, 5000);
}

#[test]
fn demotion_skipped_when_second_below_filter_threshold() {
    let c = cfg_demotion_on();
    let mut v = vec![
        e("中", "k", 9999, 0),
        e("中", "khkg", 5000, 1),
        e("罕见词", "khkg", 150, 2), // < filter_threshold(200)
    ];
    v[0].shortcode_level = 1;
    assert!(apply_demotion(&mut v, &c).is_empty());
}

#[test]
fn demotion_requires_the_char_to_have_a_shortcode() {
    let c = cfg_demotion_on();
    // 没有任何简码指向「中」→ 4 码首选是它唯一的打法，不能让
    let mut v = vec![e("中", "khkg", 5000, 0), e("口中", "khkg", 4500, 1)];
    assert!(apply_demotion(&mut v, &c).is_empty());
    assert_eq!(v[0].weight, 5000);
}

#[test]
fn conflict_detects_shortcode_vs_full4() {
    // 3 简码「khk」与 4 码「khkg」首选同为「中」
    let mut v = vec![
        e("中", "khk", 9000, 0),
        e("中", "khkg", 5000, 1),
        e("口中", "khkg", 100, 2),
    ];
    v[0].shortcode_level = 3;
    let c = analyze_conflicts(&v);
    assert!(
        c.iter()
            .any(|x| x.kind == "level3_full4" && x.long_code == "khkg"),
        "同字占 3 简码与 4 码首选应被识别为冲突: {c:?}"
    );
}

/// 降权与冲突报告的判据范围**不一致**，这是原版行为，不是疏漏：
/// `apply_demotion` 查 1/2/3 简码前缀，`analyze_conflicts` 只查 2/3。
/// 一级简码仅 25 个且基本都另有 2/3 简码，列进报告只是噪音；但降权仍要照顾它们。
/// 两处将来若要改判据范围，必须同时改——故在此钉住当前的不对称。
#[test]
fn level1_prefix_demotes_but_is_not_reported_as_conflict() {
    let c = cfg_demotion_on();
    let mut v = vec![
        e("中", "k", 9999, 0),      // 一级简码
        e("中", "khkg", 5000, 1),   // 同字占 4 码首选
        e("口中", "khkg", 4500, 2), // 够强的第二候选
    ];
    v[0].shortcode_level = 1;

    assert!(
        !analyze_conflicts(&v).iter().any(|x| x.long_code == "khkg"),
        "一级简码前缀不进冲突报告"
    );
    assert_eq!(apply_demotion(&mut v, &c).len(), 1, "但降权仍会处理它");
}

#[test]
fn conflict_output_is_deterministic() {
    let mut v = Vec::new();
    for (i, ch) in ["甲", "乙", "丙", "丁"].iter().enumerate() {
        v.push(e(ch, "k", 9000 - i as i64, i));
        v.push(e(
            ch,
            &format!("kh{}g", (b'a' + i as u8) as char),
            5000,
            i + 10,
        ));
    }
    for x in v.iter_mut().filter(|x| x.code == "k") {
        x.shortcode_level = 1;
    }
    let a = analyze_conflicts(&v);
    let b = analyze_conflicts(&v);
    let key = |c: &[Conflict]| {
        c.iter()
            .map(|x| format!("{}|{}", x.kind, x.long_code))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&a), key(&b), "同输入必须产出同顺序");
}
