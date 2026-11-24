use super::*;

fn spec(key: &str, order: i32) -> ClassSpec {
    ClassSpec {
        key: key.into(),
        name: key.into(),
        order,
        ..Default::default()
    }
}

fn with_ranges(key: &str, order: i32, r: &[(u32, u32)], common: Option<bool>) -> ClassSpec {
    ClassSpec {
        ranges: r.to_vec(),
        default_common: common,
        ..spec(key, order)
    }
}

/// 朴素线性实现，作为分段表的对照物。与 `block_of` 换二分时用的同一种守门。
fn linear_verdict(specs: &[ClassSpec], cluster: &str) -> Option<bool> {
    let ch = cluster.chars().next()?;
    let c = ch as u32;
    let mut ordered: Vec<&ClassSpec> = specs.iter().collect();
    ordered.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
    for cl in ordered {
        let hit = !cl.excluded.contains(cluster)
            && (cl.members.contains(cluster)
                || cl.ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&c)));
        if hit {
            if let Some(v) = cl.default_common {
                return Some(v);
            }
            continue;
        }
        if let Some(sc) = cl.scope
            && sc.contains(ch)
            && let Some(v) = cl.outside_common
        {
            return Some(v);
        }
    }
    None
}

#[test]
fn segments_are_sorted_and_disjoint() {
    let (reg, _) = CharsetRegistry::compile(vec![
        with_ranges("a", 1, &[(0x20, 0x7F), (0x2600, 0x26FF)], Some(false)),
        with_ranges("b", 2, &[(0x50, 0x2000)], Some(true)),
    ]);
    for w in reg.segments.windows(2) {
        assert!(w[0].end < w[1].start, "段必须有序且不相交: {w:?}");
    }
    assert!(reg.segments.iter().all(|s| s.start <= s.end));
}

/// ★ 分段表与朴素扫描必须逐码位等价——这条把「切段」这个优化钉成正确性前提。
#[test]
fn segment_table_matches_linear_scan() {
    let specs = vec![
        with_ranges("x", 1, &[(0x20, 0x7F), (0x2600, 0x26FF)], Some(false)),
        with_ranges("y", 2, &[(0x50, 0x2700)], Some(true)),
        with_ranges("z", 0, &[(0x2650, 0x2660)], None), // 命中但不表态
    ];
    let (reg, _) = CharsetRegistry::compile(specs.clone());
    for c in (0x00u32..0x3000).filter_map(char::from_u32) {
        let s = c.to_string();
        assert_eq!(
            reg.verdict_of(&s),
            linear_verdict(&specs, &s),
            "码位 U+{:04X} 两种实现不一致",
            c as u32
        );
    }
}

/// ★★ 「命中但不表态」必须继续往下让：内置区块类只为类型列存在，
/// 它们挡住后面真正表态的类就是一次静默的判定丢失。
#[test]
fn a_hit_without_opinion_defers_to_the_next_class() {
    let (reg, _) = CharsetRegistry::compile(vec![
        with_ranges("块", 1, &[(0x2600, 0x26FF)], None),
        with_ranges("emoji", 2, &[(0x2600, 0x26FF)], Some(false)),
    ]);
    assert_eq!(reg.verdict_of("\u{2600}"), Some(false));
    assert_eq!(
        reg.class_of("\u{2600}").unwrap().key,
        "emoji",
        "类型列给仲裁赢家"
    );
}

#[test]
fn smaller_order_wins() {
    let (reg, _) = CharsetRegistry::compile(vec![
        with_ranges("late", 9, &[(0x100, 0x200)], Some(true)),
        with_ranges("early", 1, &[(0x100, 0x200)], Some(false)),
    ]);
    assert_eq!(reg.verdict_of("\u{150}"), Some(false));
}

/// `remove` 要盖过 `ranges` 的命中，否则「把这个符号从类里去掉」做不到。
#[test]
fn excluded_beats_ranges_and_members() {
    let mut s = with_ranges("t", 1, &[(0x2600, 0x26FF)], Some(false));
    s.members.insert("★".into());
    s.excluded.insert("\u{2601}".into());
    s.excluded.insert("★".into());
    let (reg, _) = CharsetRegistry::compile(vec![s]);
    assert_eq!(reg.verdict_of("\u{2600}"), Some(false), "未排除的照常命中");
    assert_eq!(reg.verdict_of("\u{2601}"), None, "排除的不命中");
    assert_eq!(reg.verdict_of("★"), None, "排除优先于 members");
}

/// ★★ 作用域：「是汉字却不在字表里 ⇒ 生僻」——成员关系表达不了的那一半。
#[test]
fn scope_covers_the_complement_of_the_member_list() {
    let mut s = spec("common_han", 1);
    s.scope = Some(Scope::Han);
    s.members.insert("我".into());
    s.default_common = Some(true);
    s.outside_common = Some(false);
    let (reg, _) = CharsetRegistry::compile(vec![s]);
    assert_eq!(reg.verdict_of("我"), Some(true), "在名单里");
    assert_eq!(reg.verdict_of("龘"), Some(false), "是汉字、不在名单 ⇒ 生僻");
    assert_eq!(reg.verdict_of("A"), None, "域外不表态，留给兜底");
}

/// ★★★ 并集不是仲裁：两个类对 no_freq 意见相反时，命中任一为真即为真。
#[test]
fn no_freq_is_a_union_not_an_arbitration() {
    let mut emoji = with_ranges("emoji", 9, &[(0x25B6, 0x25B6)], Some(false));
    emoji.no_freq = true;
    let symbols = with_ranges("符号", 1, &[(0x2190, 0x2BFF)], Some(true));
    let (reg, _) = CharsetRegistry::compile(vec![emoji, symbols]);

    assert_eq!(
        reg.verdict_of("\u{25B6}"),
        Some(true),
        "常用性按 order 仲裁"
    );
    assert!(
        reg.no_freq("\u{25B6}"),
        "★ 免词频按并集——order 更靠前的「符号」没有 no_freq 也不该压掉 emoji 的"
    );
}

/// 存在性：一串文本里只要有一个字符命中就算。
#[test]
fn union_queries_are_existential_over_the_text() {
    let mut s = with_ranges("e", 1, &[(0x1F300, 0x1FAFF)], Some(false));
    s.no_freq = true;
    let (reg, _) = CharsetRegistry::compile(vec![s]);
    assert!(reg.no_freq("我\u{1F34E}你"));
    assert!(!reg.no_freq("我你"));
}

/// 多码位簇在字表里是一整条，按 char 拆开就查不到——按簇遍历钉住这一点。
#[test]
fn multi_codepoint_clusters_are_matched_whole() {
    let mut s = spec("e", 1);
    s.members.insert("1\u{FE0F}\u{20E3}".into());
    s.default_common = Some(false);
    s.no_freq = true;
    let (reg, _) = CharsetRegistry::compile(vec![s]);
    assert_eq!(reg.verdict_of("1\u{FE0F}\u{20E3}"), Some(false));
    assert!(reg.no_freq("按1\u{FE0F}\u{20E3}键"));
    assert_eq!(reg.verdict_of("1"), None, "裸数字不该命中");
}

/// 同 order 时按 key 定序：否则同一份配置每次启动可能编译出不同的仲裁结果。
#[test]
fn equal_order_is_broken_deterministically_by_key() {
    // 同一组 (key, default)，只改**输入顺序**——若把 key 与 default 的配对一起对调，
    // 测的就成了「换个类赢」而不是「换个输入顺序」，那两者本就该给出不同答案。
    let build = |reversed: bool| {
        let mut specs = vec![
            with_ranges("a", 5, &[(0x100, 0x200)], Some(true)),
            with_ranges("b", 5, &[(0x100, 0x200)], Some(false)),
        ];
        if reversed {
            specs.reverse();
        }
        let (r, _) = CharsetRegistry::compile(specs);
        r.verdict_of("\u{150}")
    };
    assert_eq!(build(false), build(true), "编译结果必须与输入顺序无关");
    assert_eq!(build(false), Some(true), "同 order 时 key 小的赢");
}

/// 位集塞不下时必须把类**丢弃并报出来**，不能静默截断——高位类恒不命中、
/// 配了没反应，是最难查的一类故障。
#[test]
fn classes_beyond_the_bitset_width_are_reported() {
    let specs: Vec<ClassSpec> = (0..MAX_CLASSES + 3)
        .map(|i| with_ranges(&format!("c{i:03}"), i as i32, &[(0x100, 0x200)], Some(true)))
        .collect();
    let (reg, dropped) = CharsetRegistry::compile(specs);
    assert_eq!(reg.classes().len(), MAX_CLASSES);
    assert_eq!(dropped.len(), 3, "超出的必须被报出来");
}

#[test]
fn detects_a_fully_shadowed_class() {
    let (reg, _) = CharsetRegistry::compile(vec![
        with_ranges("wide", 1, &[(0x100, 0x900)], Some(true)),
        with_ranges("inner", 2, &[(0x200, 0x300)], Some(false)),
        with_ranges("free", 3, &[(0x1000, 0x1100)], Some(false)),
    ]);
    assert_eq!(reg.shadowed_keys(), vec!["inner"]);
}

#[test]
fn empty_registry_is_inert() {
    let (reg, dropped) = CharsetRegistry::compile(Vec::new());
    assert!(dropped.is_empty());
    assert_eq!(reg.verdict_of("我"), None);
    assert!(!reg.no_freq("我"));
    assert!(reg.class_of("我").is_none());
}

/// ★ `no_freq` / `in_rare` 的掩码必须在**排序之后**算——位是按 `classes` 的下标建的，
/// 而排序会改变下标。算早了就位错人，表现为「开了免词频的是 A，实际免的是 B」。
///
/// 构造：`late` 开着 no_freq 但 order 靠后，`early` 没开却排在前面。排序前 `late`
/// 在下标 0，排序后在下标 1；掩码算早了会置位 0，于是 `early` 的字符被误判免词频。
#[test]
fn union_masks_are_computed_after_sorting() {
    let late = ClassSpec {
        key: "late".into(),
        ranges: vec![(0x41, 0x41)], // 'A'
        order: 100,
        no_freq: true,
        ..Default::default()
    };
    let early = ClassSpec {
        key: "early".into(),
        ranges: vec![(0x42, 0x42)], // 'B'
        order: 1,
        ..Default::default()
    };
    let (reg, _) = CharsetRegistry::compile(vec![late, early]);
    assert!(reg.no_freq("A"), "开了 no_freq 的那个类没生效");
    assert!(!reg.no_freq("B"), "没开 no_freq 的类被误判——掩码位错人了");
}

/// ★★ 裸基字符 + 变体选择符（`⚽️` = `⚽` + `U+FE0F`）必须命中。
///
/// 字表里只有基字符——生成 `emoji.yaml` 时刻意不列变体序列与 ZWJ 组合（否则文件
/// 大 6 倍），前提是消费方会逐 `char` 回落。上游词库存的恰恰多是这种裸码位 +
/// 变体符的形态，漏了就是「配了免词频，可有些 emoji 还在记词频」。
#[test]
fn a_variation_sequence_falls_back_to_its_base_character() {
    let (reg, _) = CharsetRegistry::compile(vec![ClassSpec {
        key: "e".into(),
        members: ["⚽".to_string()].into_iter().collect(),
        no_freq: true,
        ..Default::default()
    }]);
    assert!(reg.no_freq("⚽"), "基字符本身该命中");
    assert!(
        reg.no_freq("⚽\u{FE0F}"),
        "基字符 + 变体选择符——字表不列这种组合，靠逐 char 回落"
    );
}

/// 整簇优先于回落：字表里有整条 ZWJ 序列时按整簇命中，不必拆。
#[test]
fn a_whole_cluster_in_the_table_matches_as_one() {
    let (reg, _) = CharsetRegistry::compile(vec![ClassSpec {
        key: "e".into(),
        members: ["👨\u{200D}👩\u{200D}👧".to_string()].into_iter().collect(),
        no_freq: true,
        ..Default::default()
    }]);
    assert!(reg.no_freq("👨\u{200D}👩\u{200D}👧"));
    // 首码位单独出现时不在表里 ⇒ 不命中（这一条同时证明上一条不是靠回落蒙对的）。
    assert!(!reg.no_freq("👨"));
}

/// `excluded` 对回落路径同样优先：删掉基字符后，变体序列也不再命中。
#[test]
fn removing_the_base_also_kills_the_variation_sequence() {
    let (reg, _) = CharsetRegistry::compile(vec![ClassSpec {
        key: "e".into(),
        members: ["⚽".to_string()].into_iter().collect(),
        excluded: ["⚽".to_string()].into_iter().collect(),
        no_freq: true,
        ..Default::default()
    }]);
    assert!(!reg.no_freq("⚽"));
    assert!(!reg.no_freq("⚽\u{FE0F}"), "删除必须挡住回落那一步");
}
