use super::*;

fn cfg(f: impl FnOnce(&mut FuzzyConfig)) -> FuzzyConfig {
    let mut c = FuzzyConfig::default();
    f(&mut c);
    c
}

fn syls(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- shared_key

fn shared(pairs: &[(&str, &str)]) -> FuzzyConfig {
    let mut c = FuzzyConfig::default();
    for (a, b) in pairs {
        c.shared_key
            .entry(a.to_string())
            .or_default()
            .push(b.to_string());
        c.shared_key
            .entry(b.to_string())
            .or_default()
            .push(a.to_string());
    }
    c
}

/// 同键音节计 0 处，且用户一个模糊组都没开也展开（`any_enabled` 不早退）。
#[test]
fn shared_key_variant_is_zero_penalty_without_any_group() {
    let c = shared(&[("diu", "dui")]);
    assert!(c.any_enabled(), "只有同键音节时也须算作开启");
    assert_eq!(
        FuzzyMatcher::fuzzy_variants_scored("diu", &c),
        vec![("dui".to_string(), 0)]
    );
    assert_eq!(expand(&["diu"], &c), vec![("dui".to_string(), 0)]);
    assert!(fuzzy_spellings(&c).is_empty(), "同键音节都合法，不进错音串");
}

/// 与用户模糊组叠加：同键对端零罚，用户组照旧计处数；对端的用户组变体也算进来。
#[test]
fn shared_key_stacks_with_user_groups() {
    let mut c = shared(&[("lin", "liu")]);
    c.in_ing = true;
    c.n_l = true;
    let v = FuzzyMatcher::fuzzy_variants_scored("lin", &c);
    let edits = |s: &str| v.iter().find(|(x, _)| x == s).map(|(_, e)| *e);
    assert_eq!(edits("liu"), Some(0), "{v:?}");
    assert_eq!(edits("ling"), Some(1), "{v:?}");
    assert_eq!(edits("nin"), Some(1), "{v:?}");
    assert_eq!(edits("ning"), Some(2), "{v:?}");
    assert_eq!(edits("niu"), Some(1), "对端经 n_l 计 1 处: {v:?}");
    assert_eq!(edits("lin"), None, "不含自身");
    assert_eq!(v.len(), 5, "不重复: {v:?}");
}

// ---------------------------------------------------------------- correction

fn with_correction(on: bool) -> FuzzyConfig {
    let mut c = wind_config::CorrectionConfig {
        enabled: on,
        ..Default::default()
    };
    c.rules.gn_ng = true; // 出厂关；用例是 gogn / zogn
    cfg(|f| f.correction = Arc::new(CorrectionRules::from_config(&c)))
}

/// 纠错音节按 `CORRECTION_EDITS` 处计罚、不再做模糊组展开；关纠错时不出现。
#[test]
fn correction_variant_carries_heavy_edits() {
    let on = with_correction(true);
    assert!(on.any_enabled(), "只开纠错也须算作开启");
    assert!(
        FuzzyMatcher::fuzzy_variants_scored("gogn", &on)
            .contains(&("gong".to_string(), CORRECTION_EDITS)),
    );
    assert!(fuzzy_spellings(&on).contains(&"gogn".to_string()));
    let off = with_correction(false);
    assert!(
        !FuzzyMatcher::fuzzy_variants_scored("gogn", &off)
            .iter()
            .any(|(v, _)| v == "gong")
    );
    assert!(!off.any_enabled());
}

/// 纠错得到的音节不再经模糊组展开（同一音节上纠错与模糊不叠加）。
#[test]
fn correction_does_not_stack_with_fuzzy_groups() {
    let mut c = with_correction(true);
    c.zh_z = true;
    let v = FuzzyMatcher::fuzzy_variants_scored("zogn", &c);
    assert!(v.contains(&("zong".to_string(), CORRECTION_EDITS)), "{v:?}");
    assert!(!v.iter().any(|(s, _)| s == "zhong"), "{v:?}");
}

// ---------------------------------------------------------------- any_enabled

#[test]
fn any_enabled_reflects_each_group() {
    assert!(!FuzzyConfig::default().any_enabled(), "默认全关");
    assert!(cfg(|c| c.zh_z = true).any_enabled());
    assert!(cfg(|c| c.uan_uang = true).any_enabled());
    assert!(cfg(|c| c.un_ong = true).any_enabled());
    assert!(cfg(|c| c.eng_ong = true).any_enabled(), "末位组也须被算上");
}

// ------------------------------------------------------------ retroflex_relaxed

/// 三个卷舌组各管各的，且只认自己那一组——`zh_z` 开着不该让 `ch` 段跟着放宽。
#[test]
fn retroflex_relaxed_is_per_group() {
    for (head, on) in [
        (
            'z',
            (|c: &mut FuzzyConfig| c.zh_z = true) as fn(&mut FuzzyConfig),
        ),
        ('c', |c| c.ch_c = true),
        ('s', |c| c.sh_s = true),
    ] {
        assert!(
            !retroflex_relaxed(head, &FuzzyConfig::default()),
            "{head}: 全关时不放宽"
        );
        assert!(retroflex_relaxed(head, &cfg(on)), "{head}: 开了本组即放宽");
    }
    assert!(
        !retroflex_relaxed('c', &cfg(|c| c.zh_z = true)),
        "zh_z 不该带动 ch"
    );
    // 非卷舌声母没有「双字母」这回事，任何组合都不成立。
    assert!(!retroflex_relaxed('n', &cfg(|c| c.n_l = true)));
}

/// 判据不得依赖组表把长的那端写在 `a` 还是 `b`（无序对），也不得与
/// [`initials_fuzzy_equal`] 混用——后者对 zh/z 这类恒返回 true（首字母相同），
/// 拿它当判据等于开关形同虚设。
#[test]
fn retroflex_relaxed_differs_from_initials_fuzzy_equal() {
    let off = FuzzyConfig::default();
    assert!(
        initials_fuzzy_equal('z', 'z', &off),
        "首字母相同 ⇒ 恒真，这正是不能拿它当卷舌判据的原因"
    );
    assert!(!retroflex_relaxed('z', &off), "而本函数在全关时必须为假");
    // 两端对称：三组都以同一个字母开头、长度一长一短。
    for g in INITIAL_GROUPS.iter().filter(|g| g.a.len() == 2) {
        assert_eq!(
            g.a.chars().next(),
            g.b.chars().next(),
            "卷舌组两端首字母必须相同，否则 retroflex_relaxed 的形状要重估"
        );
    }
}

// ---------------------------------------------------------------- fuzzy_spellings

#[test]
fn fuzzy_spellings_empty_when_all_disabled() {
    assert!(fuzzy_spellings(&FuzzyConfig::default()).is_empty());
}

/// 只收不是合法音节的拼写：`jin`→`jing` 那侧本就切得出来，不必注册。
#[test]
fn fuzzy_spellings_only_collects_invalid_ones() {
    let out = fuzzy_spellings(&cfg(|c| c.in_ing = true));
    assert!(
        out.contains(&"tin".to_string()),
        "tin 须注册，实际: {out:?}"
    );
    assert!(
        out.contains(&"din".to_string()),
        "din 须注册，实际: {out:?}"
    );
    assert!(
        !out.iter().any(|s| VALID_SYLLABLES.contains(s.as_str())),
        "合法音节不该出现在模糊拼写表里，实际: {out:?}"
    );
    // in_ing 组在标准音节表上的缺口恰好只有 t/d 两个声母。
    assert_eq!(out, vec!["din".to_string(), "tin".to_string()]);
}

/// 声母组同样有缺口，且比韵母组更常见：平翘舌不分的人打「装置」正是 `zuangzhi`。
#[test]
fn fuzzy_spellings_covers_initial_groups() {
    let out = fuzzy_spellings(&cfg(|c| c.zh_z = true));
    assert_eq!(
        out,
        vec!["zua".to_string(), "zuai".to_string(), "zuang".to_string()],
        "zh_z 组的缺口"
    );
}

/// 只登记开启了的组 —— 关掉的组一条都不该混进来。
#[test]
fn fuzzy_spellings_respects_enabled_groups() {
    let out = fuzzy_spellings(&cfg(|c| c.in_ing = true));
    assert!(
        !out.contains(&"zuang".to_string()),
        "没开 zh_z 就不该有 zuang，实际: {out:?}"
    );
}

// ------------------------------------------------------------ 多音节整码展开

/// 按给定切分展开整码的模糊变体（不含原码），各取改动最少的形态：
/// `(变体码, 改动处数, 变体边界)`。走生产同一份 `LatticeBuilder::fuzzy_spans`。
fn expand_masked(syllables: &[&str], c: &FuzzyConfig) -> Vec<(String, usize, u64)> {
    use super::super::dag::SegGraph;
    use super::super::lattice::{EdgeVariants, LatticeBuilder};
    let syllables = syls(syllables);
    let code = syllables.concat();
    let graph = SegGraph::from_syllables(&syllables);
    LatticeBuilder::new()
        .fuzzy_spans(
            0,
            &code,
            &graph,
            c,
            &mut EdgeVariants::new(),
            code.len(),
            code.len(),
            &|_| true,
        )
        .into_iter()
        .filter(|((q, _), _)| *q == code.len())
        .map(|((_, variant), shapes)| {
            let (mask, _, edits) = shapes.into_iter().min_by_key(|s| s.2).unwrap();
            (variant, edits, mask)
        })
        .collect()
}

fn expand(syllables: &[&str], c: &FuzzyConfig) -> Vec<(String, usize)> {
    expand_masked(syllables, c)
        .into_iter()
        .map(|(v, edits, _)| (v, edits))
        .collect()
}

/// 测试辅助：只取变体码，丢掉改动处数。
fn codes(out: &[(String, usize)]) -> Vec<String> {
    out.iter().map(|(c, _)| c.clone()).collect()
}

/// 测试辅助：查某个变体码对应的改动处数。
fn fuzzy_count_of(out: &[(String, usize)], code: &str) -> Option<usize> {
    out.iter().find(|(c, _)| c == code).map(|(_, k)| *k)
}

/// 变体码的边界按变体音节的长度记（`zong|guo` → `zhong|guo` 是 {0,5} 而非 {0,4}）。
#[test]
fn expand_tracks_variant_boundaries() {
    let out = expand_masked(&["zong", "guo"], &cfg(|c| c.zh_z = true));
    assert!(
        out.contains(&("zhongguo".to_string(), 1, 0b100001)),
        "{out:?}"
    );
}

/// 原码不在结果里：那是精确命中，由各自的精确分支处理。
#[test]
fn expand_excludes_original_code() {
    let c = cfg(|c| {
        c.zh_z = true;
        c.sh_s = true;
    });
    let out = expand(&["zhong", "guo"], &c);
    assert!(!codes(&out).contains(&"zhongguo".to_string()), "{out:?}");
    assert!(!out.is_empty());
}

#[test]
fn expand_disabled_yields_nothing() {
    assert!(expand(&["si", "jin"], &FuzzyConfig::default()).is_empty());
}

#[test]
fn expand_single_syllable() {
    let out = expand(&["si"], &cfg(|c| c.sh_s = true));
    assert_eq!(
        fuzzy_count_of(&out, "shi"),
        Some(1),
        "单音节 s→sh 须计 1 处，实际: {out:?}"
    );
}

/// 非首音节的声母变体必须能生成：`zhongzou` → 「中州」需要第 2 音节 zou→zhou。
#[test]
fn expand_covers_non_initial_syllable_initial() {
    let out = expand(&["zhong", "zou"], &cfg(|c| c.zh_z = true));
    assert_eq!(
        fuzzy_count_of(&out, "zhongzhou"),
        Some(1),
        "第 2 音节 zou→zhou 须被展开并计 1，实际: {out:?}"
    );
}

/// 非首音节的韵母变体：`beijin` → `beijing`（第 2 音节 in→ing）。
#[test]
fn expand_covers_non_initial_syllable_final() {
    let out = expand(&["bei", "jin"], &cfg(|c| c.in_ing = true));
    assert_eq!(
        fuzzy_count_of(&out, "beijing"),
        Some(1),
        "第 2 音节 in→ing 须被展开并计 1，实际: {out:?}"
    );
}

/// 多处音节同时模糊：`beijinsi` → `beijingshi`（北京市）需要第 2 音节 in→ing
/// 且第 3 音节 s→sh。
///
/// 计数须为 2：惩罚按改动处数累乘（`0.5^2`），对齐 librime `kFuzzySpellingPenalty` 与
/// libime `fuzzyCost` 的逐个累加。
#[test]
fn expand_covers_multiple_syllables_at_once() {
    let c = cfg(|c| {
        c.in_ing = true;
        c.sh_s = true;
    });
    let out = expand(&["bei", "jin", "si"], &c);
    assert_eq!(
        fuzzy_count_of(&out, "beijingshi"),
        Some(2),
        "第 2、3 音节同时变体须计 2，实际: {out:?}"
    );
    // 同一次展开里，只改一个音节的组合计 1 —— 与上面合看才说明计数真在数改动，
    // 而非「只要有模糊就置 1」或「恒等于音节总数」。
    assert_eq!(fuzzy_count_of(&out, "beijingsi"), Some(1));
    assert_eq!(fuzzy_count_of(&out, "beijinshi"), Some(1));
}

/// 对多音节拼接串整体调 `fuzzy_variants` 拿不到非首音节的声母变体（声母规则是
/// `starts_with`），多音节必须逐音节展开。
#[test]
fn whole_string_variants_miss_non_initial_initials() {
    let c = cfg(|c| {
        c.in_ing = true;
        c.sh_s = true;
    });
    let whole = FuzzyMatcher::fuzzy_variants("beijinsi", &c);
    assert!(
        !whole.contains(&"beijingshi".to_string()),
        "整串调用本就拿不到非首音节声母变体（已知限制），实际: {whole:?}"
    );

    // 而逐音节展开可以——两者的差集正是逐音节展开的价值。
    let per_syllable = expand(&["bei", "jin", "si"], &c);
    assert!(codes(&per_syllable).contains(&"beijingshi".to_string()));
}

/// 组合超出每层预算时按改动处数截断，而非整体放弃：「只错一个音节」的组合全部保住。
#[test]
fn expand_keeps_fewest_edits_beyond_layer_budget() {
    let c = cfg(|c| c.in_ing = true);
    // 每个 "jin" 有 2 个选项 → 10 个音节全展开 2^10 = 1024，远超每层 256。
    let out = expand(&["jin"; 10], &c);
    assert!(out.len() < 256, "截断后仍须守住预算: {}", out.len());
    assert_eq!(
        out.iter().filter(|(_, k)| *k == 1).count(),
        10,
        "10 个「只错一个音节」的组合须全部保住: {out:?}"
    );
}

// ------------------------------------------------- 音节内声母 × 韵母的交叉组合

/// 同一音节里声母与韵母同时模糊：`senxiao` → 「生肖」需要 `sen` 一次性走完 s→sh 且 en→eng。
/// 声母规则与韵母规则各自作用于原始输入的话，只能产出 [`shen`, `seng`]，独缺 `sheng`。
#[test]
fn variants_cross_initial_and_final_within_one_syllable() {
    let c = cfg(|c| {
        c.sh_s = true;
        c.en_eng = true;
    });
    let out = FuzzyMatcher::fuzzy_variants_scored("sen", &c);
    assert_eq!(
        out.iter().find(|(v, _)| v == "sheng").map(|(_, k)| *k),
        Some(2),
        "sen→sheng 须产出且计 2 处改动，实际: {out:?}"
    );
    // 单改一处的两个变体同时保留，且各计 1。
    assert_eq!(
        out.iter().find(|(v, _)| v == "shen").map(|(_, k)| *k),
        Some(1)
    );
    assert_eq!(
        out.iter().find(|(v, _)| v == "seng").map(|(_, k)| *k),
        Some(1)
    );

    // 端到端：整码 senxiao 须能展开出 shengxiao。
    let expanded = expand(&["sen", "xiao"], &c);
    assert_eq!(
        expanded
            .iter()
            .find(|(v, _)| v == "shengxiao")
            .map(|(_, k)| *k),
        Some(2),
        "senxiao 须展开出 shengxiao，实际: {expanded:?}"
    );
}

/// 反向同样成立（词库侧/用户侧对称）：`sheng` → `sen`。
#[test]
fn variants_cross_is_symmetric() {
    let c = cfg(|c| {
        c.sh_s = true;
        c.en_eng = true;
    });
    let v = FuzzyMatcher::fuzzy_variants("sheng", &c);
    assert!(v.contains(&"sen".to_string()), "实际: {v:?}");
}

/// 声母表须按长度降序匹配：`sheng` 的声母是 `sh` 而非 `s`。否则 `sh→s` 与 `s→sh` 都对
/// `sheng` 成立会造出 `shheng`，韵母 `find("en")` 在中间命中又造出 `shengg`——这些非法码
/// 查不到词，只白白吃掉组合预算。
#[test]
fn variants_produce_no_garbage_codes() {
    let c = cfg(|c| {
        c.sh_s = true;
        c.en_eng = true;
    });
    let v = FuzzyMatcher::fuzzy_variants("sheng", &c);
    assert!(
        !v.contains(&"shheng".to_string()),
        "s→sh 不该再命中 sh 开头: {v:?}"
    );
    assert!(
        !v.contains(&"shengg".to_string()),
        "韵母须整体比较而非 find: {v:?}"
    );
}

/// 韵母整体相等比较，不做子串查找：只开 `an_ang` 时，`jiang`（韵母 iang）不该变成
/// `jian` —— 那是 `ian_iang` 组的事，用户根本没开。`shuang`→`shuan` 同理。
#[test]
fn final_rules_do_not_leak_across_groups() {
    let c = cfg(|c| c.an_ang = true);
    assert!(
        FuzzyMatcher::fuzzy_variants("jiang", &c).is_empty(),
        "只开 an_ang 时 jiang 不该有变体"
    );
    assert!(
        FuzzyMatcher::fuzzy_variants("shuang", &c).is_empty(),
        "只开 an_ang 时 shuang 不该有变体"
    );
    // 而真正属于 an_ang 组的音节照常工作。
    assert_eq!(
        FuzzyMatcher::fuzzy_variants("san", &c),
        vec!["sang".to_string()]
    );
}

/// `y`/`w` 必须计入声母表：否则 `yin` 的韵母成了整串 `yin`、与 `in` 不等，
/// `yin`→`ying` 就出不来。
#[test]
fn zero_consonant_initials_still_match_finals() {
    let c = cfg(|c| c.in_ing = true);
    assert!(FuzzyMatcher::fuzzy_variants("yin", &c).contains(&"ying".to_string()));
    assert!(FuzzyMatcher::fuzzy_variants("ying", &c).contains(&"yin".to_string()));
    // 零声母音节（韵母即整串）
    let c2 = cfg(|c| c.en_eng = true);
    assert!(FuzzyMatcher::fuzzy_variants("en", &c2).contains(&"eng".to_string()));
}

/// 组合产物须落在合法音节集内：`juan` + uan_uang 会算出 `juang`，不是音节，须剔除。
#[test]
fn variants_reject_invalid_syllables() {
    let c = cfg(|c| c.uan_uang = true);
    assert!(
        FuzzyMatcher::fuzzy_variants("juan", &c).is_empty(),
        "juang 不是合法音节，须被剔除"
    );
    // 合法的照常保留。
    assert!(FuzzyMatcher::fuzzy_variants("zhuan", &c).contains(&"zhuang".to_string()));
}

/// 裸声母/半成品码（`s`、`sh`）不是合法音节，此时不做合法性过滤——
/// 单音节查询路径会拿未成音节的 code 直接进来，过滤会误杀既有召回。
#[test]
fn bare_initial_keeps_loose_matching() {
    let c = cfg(|c| c.sh_s = true);
    assert!(FuzzyMatcher::fuzzy_variants("s", &c).contains(&"sh".to_string()));
    assert!(FuzzyMatcher::fuzzy_variants("sh", &c).contains(&"s".to_string()));
}

#[test]
fn expand_empty_input_is_safe() {
    assert!(expand(&[], &cfg(|c| c.zh_z = true)).is_empty());
}

// ------------------------------------------------------------ un_ong / eng_ong

/// 断言 `from` 在 `c` 下的变体恰好是 `expect`（顺序无关）。
fn assert_variants(from: &str, c: &FuzzyConfig, expect: &[&str]) {
    let mut got = FuzzyMatcher::fuzzy_variants(from, c);
    got.sort();
    let mut want: Vec<String> = expect.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want, "{from} 的变体");
}

/// `un ↔ ong`：声母后接 un 的音节，凡 ong 那端合法的双向互通。
#[test]
fn un_ong_pairs_legal_syllables_both_ways() {
    let c = cfg(|c| c.un_ong = true);
    for (un, ong) in [
        ("dun", "dong"),
        ("tun", "tong"),
        ("lun", "long"),
        ("zun", "zong"),
        ("cun", "cong"),
        ("sun", "song"),
        ("zhun", "zhong"),
        ("chun", "chong"),
        ("shun", "shong"),
        ("run", "rong"),
        ("gun", "gong"),
        ("kun", "kong"),
        ("hun", "hong"),
        ("yun", "yong"),
    ] {
        if !VALID_SYLLABLES.contains(ong) {
            // shong 不是音节：shun 这一对只能被过滤（下一条测试钉住）。
            continue;
        }
        assert_variants(un, &c, &[ong]);
        assert_variants(ong, &c, &[un]);
    }
    // 关着时一条都不出。
    assert!(FuzzyMatcher::fuzzy_variants("dun", &FuzzyConfig::default()).is_empty());
}

/// `jun`/`qun`/`xun` 的韵母切出来是 `un`（`split_initial_final` 按字面切，不还原 ü），
/// 对端 `jong`/`qong`/`xong` 不是音节，须被过滤。不映射到 `jiong`/`qiong`/`xiong`
/// （那是 iong 韵，不属于本组）。`shun`→`shong` 同理。
#[test]
fn un_ong_filters_illegal_counterparts() {
    let c = cfg(|c| c.un_ong = true);
    for s in ["jun", "qun", "xun", "shun"] {
        assert_variants(s, &c, &[]);
    }
    // iong 韵不受本组影响（反向也不会产出 jun）。
    assert_variants("jiong", &c, &[]);
    assert_variants("xiong", &c, &[]);
}

/// `eng ↔ ong`：只保留两端都合法的；`feng`/`meng`/`peng`/`beng`/`weng` 的对端
/// `fong`/`mong`/`pong`/`bong`/`wong` 不是音节，须被过滤。
#[test]
fn eng_ong_pairs_only_legal_syllables() {
    let c = cfg(|c| c.eng_ong = true);
    for (eng, ong) in [
        ("deng", "dong"),
        ("teng", "tong"),
        ("neng", "nong"),
        ("leng", "long"),
        ("geng", "gong"),
        ("keng", "kong"),
        ("heng", "hong"),
        ("zeng", "zong"),
        ("ceng", "cong"),
        ("seng", "song"),
        ("zheng", "zhong"),
        ("cheng", "chong"),
        ("reng", "rong"),
    ] {
        assert_variants(eng, &c, &[ong]);
        assert_variants(ong, &c, &[eng]);
    }
    for s in ["feng", "meng", "peng", "beng", "weng", "sheng", "eng"] {
        assert_variants(s, &c, &[]);
    }
    // yong 的对端 yeng 同样不是音节。
    assert_variants("yong", &c, &[]);
}

/// `ong` 同属两组（与声母 `l` 同属 n↔l、r↔l 同形）：两组都开时两个对端都出。
/// 但不传递：只取与原韵母直接成对的对端——`dun` 不经 `ong` 走到 `deng`，
/// `den` 也不经 `eng` 走到 `dong`。
#[test]
fn ong_groups_expand_direct_pairs_only() {
    let all = cfg(|c| {
        c.en_eng = true;
        c.un_ong = true;
        c.eng_ong = true;
    });
    assert_variants("dong", &all, &["dun", "deng"]);
    assert_variants("deng", &all, &["den", "dong"]);
    assert_variants("dun", &all, &["dong"]);
    assert_variants("den", &all, &["deng"]);

    // 与声母组交叉：zhong 在 zh_z + un_ong + eng_ong 下 2×3 的笛卡尔积全合法。
    let c = cfg(|c| {
        c.zh_z = true;
        c.un_ong = true;
        c.eng_ong = true;
    });
    let out = FuzzyMatcher::fuzzy_variants_scored("zhong", &c);
    for (v, k) in [
        ("zong", 1),
        ("zhun", 1),
        ("zheng", 1),
        ("zun", 2),
        ("zeng", 2),
    ] {
        assert_eq!(
            out.iter().find(|(s, _)| s == v).map(|(_, n)| *n),
            Some(k),
            "zhong→{v} 须计 {k} 处，实际: {out:?}"
        );
    }
    assert_eq!(out.len(), 5, "实际: {out:?}");
}

/// 多音节展开：`dunxi` 在 un_ong 下出 `dongxi`（东西）。
#[test]
fn un_ong_expands_within_multi_syllable_code() {
    let c = cfg(|c| {
        c.un_ong = true;
        c.eng_ong = true;
        c.en_eng = true;
    });
    let out = expand(&["dun", "xi"], &c);
    assert_eq!(fuzzy_count_of(&out, "dongxi"), Some(1), "实际: {out:?}");
    // ong 有两个对端 ⇒ 每个 zhong 3 个选项，4 个即 81 种组合，在每层预算内全部保留
    // （扣掉原码 80 种）。
    let out = expand(&["zhong"; 4], &c);
    assert_eq!(out.len(), 80, "{out:?}");
    assert!(out.iter().any(|(_, k)| *k == 1));
}

/// 切分层：打的那端不成音节时照样注册（与 `tin`→`ting` 同一机制），但零声母的 `ong`
/// （来自 `eng`→`ong`）不注册——它会在 `xi|ong`、`ji|ong` 处凭空造出新切分。
#[test]
fn fuzzy_spellings_for_ong_groups() {
    let un = fuzzy_spellings(&cfg(|c| c.un_ong = true));
    for s in ["jong", "qong", "xong", "shong"] {
        assert!(un.contains(&s.to_string()), "{s} 须注册，实际: {un:?}");
    }
    let eng = fuzzy_spellings(&cfg(|c| c.eng_ong = true));
    for s in ["fong", "mong", "pong", "bong", "wong", "yeng"] {
        assert!(eng.contains(&s.to_string()), "{s} 须注册，实际: {eng:?}");
    }
    assert!(
        !eng.contains(&"ong".to_string()),
        "零声母 ong 不得注册，实际: {eng:?}"
    );
}
