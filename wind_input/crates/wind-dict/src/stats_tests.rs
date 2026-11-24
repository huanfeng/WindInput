use super::*;

/// (层, 码, 文字, 权重, 自然序, 音节边界)
type Row = (usize, &'static str, &'static str, i32, i32, u64);

/// 把夹具行按码分组（组内保持层序 → 行序），喂给 builder。
fn run(rows: &[Row], opts: StatsOptions<'_>) -> DictStats {
    let mut groups: BTreeMap<&str, Vec<GroupEntry<'static>>> = BTreeMap::new();
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by_key(|r| r.0);
    for &&(layer, code, text, weight, order, boundary) in &sorted {
        groups.entry(code).or_default().push(GroupEntry {
            layer,
            text,
            weight,
            order,
            boundary,
        });
    }
    let mut b = StatsBuilder::new(opts);
    for (code, es) in &groups {
        b.push_group(code, es);
    }
    b.finish()
}

fn layers(base_orders: &[i32]) -> Vec<StatsLayer> {
    base_orders
        .iter()
        .map(|&b| StatsLayer {
            base_order: b,
            ..Default::default()
        })
        .collect()
}

fn m(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn strs(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// 码表夹具：主库（档位 0）+ 扩展库（档位 1，含与主库重复的「式」、与「式」同权的「戒」、
/// 扩展 B 区字「𠀀」）+ 表情库（档位 2，emoji 与「A股」进「其他」）。
const CT: &[Row] = &[
    (0, "a", "工", 100, 0, 0),
    (0, "aaaa", "工", 50, 1, 0),
    (0, "aaaa", "式", 80, 2, 0),
    (0, "aaab", "工作", 70, 3, 0),
    (0, "b", "了", 90, 4, 0),
    (0, "bnh", "了", 60, 5, 0),
    (0, "bbbb", "子", 40, 6, 0),
    (0, "bbbb", "孑", 30, 7, 0),
    (0, "bbbb", "孓", 30, 8, 0),
    (0, "bbbb", "子孙", 20, 9, 0),
    (0, "ccc", "中华人民", 10, 10, 0),
    (0, "cccc", "中华人民共和国", 10, 11, 0),
    (0, "dd", "三个字", 5, 12, 0),
    (1, "aaaa", "式", 80, 0, 0),
    (1, "aaaa", "戒", 80, 1, 0),
    (1, "ee", "𠀀", 0, 2, 0),
    (2, "aaaa", "😀", 1, 0, 0),
    (2, "ff", "A股", 1, 1, 0),
];

fn ct_charsets() -> Vec<Charset> {
    vec![
        Charset {
            key: "l1".into(),
            label: "一级".into(),
            chars: strs(&["工", "式", "子", "了", "丢"]),
        },
        Charset {
            key: "l12".into(),
            label: "一二级".into(),
            chars: strs(&["工", "式", "子", "了", "丢", "戒", "孑", "孓", "乙"]),
        },
    ]
}

fn ct_freq() -> HashMap<char, u64> {
    [
        ('工', 100),
        ('式', 50),
        ('子', 30),
        ('了', 200),
        ('戒', 10),
        ('孑', 5),
        ('孓', 5),
        ('丢', 1000),
    ]
    .into_iter()
    .collect()
}

fn ct_stats(rank: RankOrder, ls: Vec<StatsLayer>, top_n: usize) -> DictStats {
    let cs = ct_charsets();
    let freq = ct_freq();
    run(
        CT,
        StatsOptions {
            kind: StatsKind::Codetable,
            rank,
            max_code_length: 4,
            top_n,
            layers: ls,
            main_layer: Some(0),
            freq: Some(&freq),
            charsets: &cs,
        },
    )
}

#[test]
fn codetable_basic_counts() {
    let s = ct_stats(RankOrder::Weight, layers(&[0, 1, 2]), 10);
    assert_eq!(
        s.layers,
        vec![
            LayerCounts {
                entries: 13,
                chars: 8,
                words: 5,
                other: 0
            },
            LayerCounts {
                entries: 3,
                chars: 3,
                words: 0,
                other: 0
            },
            LayerCounts {
                entries: 2,
                chars: 0,
                words: 0,
                other: 2
            },
        ]
    );
    assert_eq!(
        s.total,
        TotalCounts {
            entries: 17, // 18 行，扩展库的「式 aaaa」与主库重复
            unique_chars: 8,
            unique_words: 5,
            by_len: m(&[("1", 10), ("2", 2), ("3", 1), ("4", 1), ("5+", 1)]),
            other: 2,
            multi_code_chars: 2, // 工（a/aaaa）、了（b/bnh）
        }
    );
    assert!(s.pinyin.is_none());
}

#[test]
fn codetable_code_len_and_short_codes() {
    let s = ct_stats(RankOrder::Weight, layers(&[0, 1, 2]), 10);
    assert_eq!(
        s.code_len,
        Some(CodeLenDist {
            char: m(&[("1", 2), ("2", 1), ("3", 1), ("4", 6)]),
            word: m(&[("2", 1), ("3", 1), ("4", 3)]),
        })
    );
    // 工 的全码 aaaa、了 的全码 bnh（只有 3 码也是全码）；两字各有一级简码。
    assert_eq!(
        s.short_codes,
        Some(ShortCodes {
            levels: m(&[("1", 2)]),
            chars_with_short: 2
        })
    );
}

#[test]
fn codetable_dup_groups_and_top_follow_static_rank() {
    let s = ct_stats(RankOrder::Weight, layers(&[0, 1, 2]), 10);
    // aaaa 的 😀 不算：aaaa 3（式 戒 工）、bbbb 4（子 孑 孓 子孙）
    assert_eq!((s.dup.groups, s.dup.entries, s.dup.non_first), (2, 7, 5));
    assert_eq!((s.dup.max, s.dup.max_code.as_str()), (4, "bbbb"));
    let cf = s.dup.char_full.clone().unwrap();
    assert_eq!((cf.groups, cf.chars, cf.max), (2, 6, 3));
    assert_eq!(
        cf.top,
        vec![
            DupGroup {
                code: "aaaa".into(),
                size: 3,
                texts: strs(&["式", "戒", "工"])
            },
            DupGroup {
                code: "bbbb".into(),
                size: 3,
                texts: strs(&["子", "孑", "孓"])
            },
        ]
    );
    // 主库单算：aaaa 只有工、式；bbbb 四个都在主库。
    assert_eq!(
        s.dup.main,
        Some(MainDup {
            groups: 2,
            entries: 6,
            max: 4,
            max_code: "bbbb".into()
        })
    );
    // 式、戒同权 80：档位 0 的式在前；孑、孓同权 30：自然序决定。
    assert_eq!(
        s.dup.top,
        vec![
            DupGroup {
                code: "bbbb".into(),
                size: 4,
                texts: strs(&["子", "孑", "孓", "子孙"])
            },
            DupGroup {
                code: "aaaa".into(),
                size: 3,
                texts: strs(&["式", "戒", "工"])
            },
        ]
    );
    let s1 = ct_stats(RankOrder::Weight, layers(&[0, 1, 2]), 1);
    assert_eq!(s1.dup.top.len(), 1);
    assert_eq!(s1.dup.top[0].code, "bbbb", "留最大的一组");
    assert_eq!(
        s1.dup.char_full.unwrap().top[0].code,
        "aaaa",
        "同大小按码升序"
    );
}

#[test]
fn natural_order_and_default_weight_change_rank() {
    let aaaa = |s: &DictStats| {
        s.dup
            .top
            .iter()
            .find(|g| g.code == "aaaa")
            .unwrap()
            .texts
            .clone()
    };
    let s = ct_stats(RankOrder::Natural, layers(&[0, 1, 2]), 10);
    assert_eq!(aaaa(&s), strs(&["工", "式", "戒"]));
    // 扩展库整体定档 10：戒沉到工之后；「式」取主库那条（80）。
    let mut ls = layers(&[0, 1, 2]);
    ls[1].default_weight = Some(10);
    let s = ct_stats(RankOrder::Weight, ls, 10);
    assert_eq!(aaaa(&s), strs(&["式", "工", "戒"]));
}

/// 同码下 emoji 权重更高也不占位：汉字仍是首选、不成重码组；含汉字的「T恤」照算重码。
#[test]
fn emoji_and_symbols_neither_rank_nor_make_dup_groups() {
    let rows: &[Row] = &[
        (0, "xx", "😀", 100, 0, 0),
        (0, "xx", "※", 90, 1, 0),
        (0, "xx", "汉", 10, 2, 0),
        (0, "tx", "T恤", 50, 3, 0),
        (0, "tx", "体", 40, 4, 0),
    ];
    let cs = vec![Charset {
        key: "k".into(),
        label: "K".into(),
        chars: strs(&["汉", "体"]),
    }];
    let freq: HashMap<char, u64> = [('汉', 10), ('体', 30)].into_iter().collect();
    let s = run(
        rows,
        StatsOptions {
            kind: StatsKind::Codetable,
            rank: RankOrder::Weight,
            max_code_length: 4,
            top_n: 10,
            layers: layers(&[0]),
            main_layer: Some(0),
            freq: Some(&freq),
            charsets: &cs,
        },
    );
    assert_eq!(
        (s.dup.groups, s.dup.max, s.dup.max_code.as_str()),
        (1, 2, "tx")
    );
    assert_eq!(s.dup.top[0].texts, strs(&["T恤", "体"]));
    assert_eq!(s.dup.main.as_ref().unwrap().groups, 1);
    assert_eq!(s.total.other, 3, "😀 ※ T恤 都仍计入「其他」");
    // 汉 在 xx 下是首选；体 被 T恤 压在第 2 → 选重率 30/40
    assert_eq!(s.charsets[0].dyn_rate, Some(30.0 / 40.0));
}

/// 跨库去重按去掉首尾空白的文字：「 工」与「工」是同一条，不凑成重码组。
#[test]
fn dedup_ignores_surrounding_whitespace() {
    let rows: &[Row] = &[(0, "a", " 工", 9, 0, 0), (1, "a", "工", 5, 0, 0)];
    let s = run(
        rows,
        StatsOptions {
            kind: StatsKind::Codetable,
            rank: RankOrder::Weight,
            max_code_length: 4,
            top_n: 0,
            layers: layers(&[0, 1]),
            main_layer: None,
            freq: None,
            charsets: &[],
        },
    );
    assert_eq!(
        (s.total.entries, s.dup.groups, s.total.unique_chars),
        (1, 0, 1)
    );
}

#[test]
fn charset_coverage_static_dup_and_weighted_rates() {
    let s = ct_stats(RankOrder::Weight, layers(&[0, 1, 2]), 10);
    let l1 = &s.charsets[0];
    assert_eq!((l1.size, l1.missing), (5, 1));
    assert_eq!(l1.missing_chars, strs(&["丢"]));
    assert_eq!((l1.static_dup, l1.dup_groups), (Some(1), Some(1)));
    assert_eq!(l1.dyn_rate, Some(0.0));
    // (工 100×1 + 式 50×4 + 子 30×4 + 了 200×1) / 380
    assert!((l1.avg_code_len.unwrap() - 620.0 / 380.0).abs() < 1e-12);

    let l12 = &s.charsets[1];
    assert_eq!((l12.size, l12.missing), (9, 2));
    assert_eq!((l12.static_dup, l12.dup_groups), (Some(4), Some(2)));
    // 戒 10 + 孑 5 + 孓 5 不在首选
    assert_eq!(l12.dyn_rate, Some(20.0 / 400.0));
    assert_eq!(l12.avg_code_len, Some(700.0 / 400.0));
}

#[test]
fn weighted_metrics_absent_without_freq() {
    let cs = ct_charsets();
    let s = run(
        CT,
        StatsOptions {
            kind: StatsKind::Codetable,
            rank: RankOrder::Weight,
            max_code_length: 4,
            top_n: 0,
            layers: layers(&[0, 1, 2]),
            main_layer: None,
            freq: None,
            charsets: &cs,
        },
    );
    assert!(s.dup.top.is_empty());
    assert_eq!(s.charsets[1].static_dup, Some(4), "静重不依赖字频");
    assert_eq!(
        (s.charsets[1].dyn_rate, s.charsets[1].avg_code_len),
        (None, None)
    );
}

/// 全码上限：超过 max_code_length 的怪码不当全码，也就不让真全码变成「简码」。
#[test]
fn full_code_respects_max_code_length() {
    let rows: &[Row] = &[(0, "khk", "中", 50, 0, 0), (0, "khkkhk", "中", 99, 1, 0)];
    let s = run(
        rows,
        StatsOptions {
            kind: StatsKind::Codetable,
            rank: RankOrder::Weight,
            max_code_length: 4,
            top_n: 0,
            layers: layers(&[0]),
            main_layer: None,
            freq: None,
            charsets: &[],
        },
    );
    assert_eq!(s.short_codes.unwrap().chars_with_short, 0);
    assert_eq!(s.code_len.unwrap().char, m(&[("3", 1), ("5+", 1)]));
}

#[test]
fn pinyin_polyphones_syllables_and_zero_weight() {
    let rows: &[Row] = &[
        (0, "a", "啊", 100, 0, 0b1),
        (0, "a", "阿", 50, 1, 0b1),
        (0, "e", "阿", 10, 2, 0b1),
        (0, "nihao", "你好", 80, 3, 0b101),
        (0, "xian", "先", 70, 4, 0b1),
        (0, "xian", "西安", 60, 5, 0b101),
        (0, "zhong", "中", 0, 6, 0b1),
        (0, "zhong", "种", 0, 7, 0b1),
        (0, "Agu", "A股", 1, 8, 0b11),
    ];
    let cs = vec![Charset {
        key: "s".into(),
        label: "S".into(),
        chars: strs(&["啊", "中", "丢"]),
    }];
    let s = run(
        rows,
        StatsOptions {
            kind: StatsKind::Pinyin,
            rank: RankOrder::Weight,
            max_code_length: 0,
            top_n: 2,
            layers: layers(&[0]),
            main_layer: None,
            freq: None,
            charsets: &cs,
        },
    );
    assert_eq!(s.total.unique_chars, 5);
    assert_eq!(s.total.unique_words, 2);
    assert_eq!(s.total.multi_code_chars, 1, "阿 a/e");
    assert_eq!(
        s.pinyin,
        Some(PinyinExtra {
            syllables: 9, // a e ni hao xian xi an zhong gu：「A股」的 A 不算音节
            zero_weight: 2
        })
    );
    assert_eq!(
        (s.dup.groups, s.dup.max, s.dup.max_code.as_str()),
        (3, 2, "a")
    );
    assert_eq!(
        s.dup
            .top
            .iter()
            .map(|g| g.code.as_str())
            .collect::<Vec<_>>(),
        ["a", "xian"]
    );
    assert!(s.code_len.is_none() && s.short_codes.is_none() && s.dup.char_full.is_none());
    let c = &s.charsets[0];
    assert_eq!((c.missing, c.missing_chars.clone()), (1, strs(&["丢"])));
    assert_eq!((c.static_dup, c.dyn_rate), (None, None));
}

#[test]
fn english_buckets_by_letters() {
    let rows: &[Row] = &[
        (0, "a", "a", 1, 0, 0),
        (0, "hello", "hello", 1, 1, 0),
        (0, "hi", "hi", 1, 2, 0),
    ];
    let s = run(
        rows,
        StatsOptions {
            kind: StatsKind::English,
            rank: RankOrder::Weight,
            max_code_length: 0,
            top_n: 0,
            layers: layers(&[0]),
            main_layer: None,
            freq: None,
            charsets: &[],
        },
    );
    assert_eq!(s.total.unique_words, 3);
    assert_eq!(
        s.total.by_len,
        m(&[("1", 1), ("2", 1), ("3", 0), ("4", 0), ("5+", 1)])
    );
    assert_eq!(s.total.other, 0);
}

#[test]
fn han_classification() {
    for c in ['〇', '中', '㐀', '𠀀', '\u{E000}', '豈'] {
        assert!(is_han(c), "{c}");
    }
    for c in ['A', '，', '😀', 'ā', '・'] {
        assert!(!is_han(c), "{c}");
    }
}

#[test]
fn char_freq_sums_readings_of_single_chars() {
    let mut f = HashMap::new();
    add_char_freq(
        "---\nname: x\n...\n# c\n啊\ta\t10\n啊\te\t5\n阿\ta\t3\n你好\tni hao\t9\n坏\tx\n",
        &mut f,
    );
    assert_eq!(f, [('啊', 15), ('阿', 3)].into_iter().collect());
}

#[test]
fn common_han_levels_by_section_headers() {
    let t = "key: common_han\n...\n\n# 一级（3500）\n一\n乙\n# 二级（3000）\n丐\n-乙\n# 三级（其余）\n乂\n⚽️\n";
    assert_eq!(
        parse_common_han_levels(t),
        vec![strs(&["一", "乙"]), strs(&["丐"]), strs(&["乂"])]
    );
}
