use super::*;
use wind_candidate::CandidateSource;

fn pin(text: &str, weight: i32) -> Candidate {
    Candidate {
        text: text.to_string(),
        weight,
        consumed_length: 0,
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

fn ct(code: &str, text: &str, weight: i32) -> Candidate {
    Candidate {
        code: code.to_string(),
        text: text.to_string(),
        weight,
        consumed_length: 0,
        source: CandidateSource::CodeTable,
        ..Default::default()
    }
}

fn recs(items: &[(&str, u32, i64)]) -> HashMap<String, FreqRecord> {
    items
        .iter()
        .map(|(t, c, lu)| {
            (
                t.to_string(),
                FreqRecord {
                    count: *c,
                    last_used: *lu,
                },
            )
        })
        .collect()
}

const NOW: i64 = 1_700_000_000;

fn pin_sentence(text: &str, weight: i32) -> Candidate {
    let mut c = pin(text, weight);
    c.is_sentence = true;
    c
}

/// 整句不锚定：同层的常用词可凭位置提升反超它。
///
/// 整句 weight 与词库同量纲，靠 weight 在 `candidate_display_order` 里挣到 base_pos 0，但不对
/// 词频免疫。无词频时整句仍居首，见对照组 `pinyin_sentence_leads_without_freq`（缺了它，
/// 「整句被无条件压到最后」的实现也能骗过本条）。
#[test]
fn pinyin_sentence_can_be_overtaken_by_frequent_peer() {
    // weight 取等效词频量级（几千），不再是旧的 3e7 —— 本函数从不比 weight，
    // 但编造一个已不存在的量级会误导读者以为整句仍在另一根轴上。
    let mut cands = vec![
        pin_sentence("你好世界", 4000),
        pin("你好", 2000),
        pin("拟", 1000),
    ];
    let r = recs(&[("你好", 20, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text, "你好",
        "选过 20 次的同层词须能提升到首位、反超整句"
    );
    assert_eq!(
        cands[1].text, "你好世界",
        "整句只是退居第二，不得被赶出列表"
    );
    assert_eq!(cands[2].text, "拟");
}

/// 零权重读音让位对有词频记录的字失效：摘掉标记、退回精确层，排到简拼词之前。
/// 无记录的对照字保留标记、留在简拼层。
#[test]
fn freq_record_clears_zero_weight_reading() {
    let zero = |text: &str| {
        let mut c = pin(text, 0);
        c.is_zero_weight_reading = true;
        c
    };
    let abbrev = {
        let mut c = pin("成为", 50000);
        c.is_abbrev = true;
        c
    };
    let mut cands = vec![abbrev, zero("𤭢"), zero("嫩")];
    let r = recs(&[("𤭢", 1, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(cands[0].text, "𤭢", "选过的字应退回精确层、排到简拼词之前");
    assert!(!cands[0].is_zero_weight_reading);
    assert_eq!(cands[1].text, "成为");
    assert!(cands[2].is_zero_weight_reading, "无词频记录的字仍应让位");
}

/// 模糊同码候选能靠词频反超整句。
///
/// 别的情形下整句本就压不住对手（同码同层的精确竞争者、或 `cmp_match_layers` 在 target_pos
/// 之前分开的跨层候选），模糊候选是整句不锚定后唯一实际改变的场景：用户明确选过的就该能赢。
#[test]
fn pinyin_fuzzy_peer_can_overtake_sentence() {
    let fuzzy_peer = {
        let mut c = pin("事实", 2000);
        c.is_fuzzy = true; // 模糊音候选：6.6 不把它算作竞争者
        c
    };
    let mut cands = vec![pin_sentence("是识", 4000), fuzzy_peer];
    let r = recs(&[("事实", 20, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text, "事实",
        "选过 20 次的模糊同码候选须能反超整句（此前被锚定硬闸门挡死）"
    );
}

/// 对照组：无词频记录时整句仍居首（靠入参显示序，即 weight）。
/// 与上一条合看才说明「整句让位」是词频造成的，而非本函数把整句一律压后。
#[test]
fn pinyin_sentence_leads_without_freq() {
    let mut cands = vec![
        pin_sentence("你好世界", 4000),
        pin("你好", 2000),
        pin("拟", 1000),
    ];
    rerank_pinyin_positional(
        &mut cands,
        &recs(&[]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(cands[0].text, "你好世界", "无词频时整句仍是最优解读");
}

/// 降级整句不参与锚定：让位于精确整词后，须停在精确整词之后、普通候选之前。
///
/// 引擎侧只改 weight，而本比较器不看 weight、标志为真就置顶；此处不豁免，引擎那半边等于白改。
///
/// 入参顺序即协调器 `candidate_display_order` 的输出（`handle_candidate.rs` 先按权重层级排好再调
/// 本函数）。本函数同层同衰减分时返回 `Ordering::Equal`、靠稳定排序保住入参序，从不自己比权重，
/// 故测试必须按显示序喂入。
#[test]
fn pinyin_demoted_sentence_yields_to_exact_word() {
    let mut cands = vec![
        pin("廉政提醒", 100_000),
        {
            // 引擎侧已把权重降到「最低的精确整词 - 1」
            let mut c = pin_sentence("连整体性", 99_999);
            c.is_sentence_demoted = true;
            c
        },
        {
            // 子短语层：无论权重多高都该留在整句之后（由 cmp_match_layers 保证）
            let mut c = pin("连", 500_000);
            c.is_partial = true;
            c
        },
    ];
    let r = recs(&[]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(cands[0].text, "廉政提醒", "精确整词须在降级整句之前");
    assert_eq!(cands[1].text, "连整体性", "降级整句仍须在普通候选之前");
    assert_eq!(cands[2].text, "连");
}

/// 整句排在哪里由入参显示序决定，本函数不会把它拉回首位。
///
/// 锁住「协调器 `candidate_display_order` 是权威显示序」：谁往 `anchored` 里加整句判据，
/// 引擎排好的序会在这里被推翻，本断言即红。
#[test]
fn pinyin_sentence_is_not_pulled_back_to_top() {
    let mut cands = vec![pin("廉政提醒", 100_000), pin_sentence("连整体性", 99_999)];
    let r = recs(&[]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text, "廉政提醒",
        "入参序即显示序，整句不得被拉回首位"
    );
    assert_eq!(cands[1].text, "连整体性");
}

/// 降级整句失去锚定后，词频学习对它生效（未降级的整句则恒锚定，见上一条）。
#[test]
fn pinyin_demoted_sentence_participates_in_freq() {
    let mut cands = vec![pin("精确整词", 100_000), {
        let mut c = pin_sentence("降级整句", 99_999);
        c.is_sentence_demoted = true;
        c
    }];
    // 需 W_freq > 100_000 才能翻过「精确整词」⇒ count > 100000/1307 ≈ 76.5。
    // 旧值 20 是布尔闸门时代的遗留——那时 count 只用来过 ε 阈值，多少都一样。
    let r = recs(&[("降级整句", 100, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text, "降级整句",
        "降级整句不再锚定，故可凭词频重新浮上来"
    );
}

/// 有同码竞争者的整句接受词频挑战：同码精确整词可凭词频反超。
///
/// `siyuan`：「寺院」既是词典精确整词、又被 Viterbi 选为最优解（step 2 同文合并继承整句身份），
/// 「思源」同码。整句不锚定，本行为不需要任何「被竞争」标记。
#[test]
fn pinyin_contested_sentence_yields_to_used_peer() {
    let mut cands = vec![pin_contested("寺院"), pin("思源", 245)];
    // 第 2 位用一次即可提升到第 0 位。
    let r = recs(&[("思源", 1, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text,
        "思源",
        "有同码竞争者的整句须接受词频挑战，实际: {:?}",
        cands.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert_eq!(cands[1].text, "寺院", "整句退居第二，不是被赶出列表");
}

/// 与上一条配对：无词频记录时同款整句仍居首。
/// 有同码竞争者本身不该压低整句；若误把「有竞争者」做成降权，这条会挂。
#[test]
fn pinyin_contested_sentence_still_leads_without_freq() {
    let mut cands = vec![pin_sentence("寺院", 491), pin("思源", 245)];
    let r = recs(&[]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text, "寺院",
        "无使用记录时整句仍是最优解读，须维持首位"
    );
}

/// 锚定按来源语义而非权重数值：高权重但非整句的候选（如别的功能提权到 30M 的词）
/// 仍须正常参与词频重排，不得被误当整句锚定而永久失去词频学习能力。
/// 这正是把 `weight >= 20M` 阈值换成 `is_sentence` 标记要解决的问题。
#[test]
fn pinyin_high_weight_non_sentence_still_learns() {
    // 92,194 = 8105 常用字表的 p99，是非整句候选够得到的真实量级。
    let mut cands = vec![pin("高权非整句", 92_194), pin("近用低权", 100)];
    let r = recs(&[("近用低权", 1, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text, "近用低权",
        "非整句候选不享锚定豁免，第 2 位用一次即可超过它"
    );

    // 配对用例：`anchored_sentence_is_immune_to_promotion` 验证锚定者用 50 次也不动，
    // `no_record_means_no_movement` 验证无记录时一步不挪。位置模型下两个候选时第 2 位
    // 用一次就到顶，不存在「次数不足」的对照。
}

/// 精确码短语（`is_phrase && is_exact_code`，lookup 完全匹配）与整句同享锚定豁免。
/// 前缀短语不锚定，见 `pinyin_prefix_phrase_not_anchored`。
#[test]
fn pinyin_exact_phrase_is_anchored_like_sentence() {
    let mut cands = vec![pin("普通词", 5000), {
        let mut c = pin("我的邮箱", 40_000_000);
        c.is_phrase = true;
        c.is_exact_code = true; // 精确码短语才锚定
        c
    }];
    let r = recs(&[("普通词", 30, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(cands[0].text, "我的邮箱", "精确码短语须与整句同享锚定");
}

/// 前缀短语不锚定：`is_phrase && !is_exact_code`（`lookup_prefix` 命中）落到匹配层，
/// 靠 is_prefix 降到精确候选之下，不因 is_phrase 被顶到首位。
///
/// 场景：开自动调频且有词频记录时打 `da`，`date` 前缀短语不得被 `|| is_phrase` 一刀切锚定到首位。
/// 入参按 candidate_display_order 输出序（精确拼音字在前、前缀短语在后）。
#[test]
fn pinyin_prefix_phrase_not_anchored() {
    let exact_word = pin("大", 5000); // 精确拼音字：is_prefix=false, is_exact_code=false
    let prefix_phrase = {
        let mut c = pin("date短语", 40_000_000); // 高权重也不该把它顶起
        c.is_phrase = true;
        c.is_prefix = true; // is_exact_code 默认 false
        c
    };
    let mut cands = vec![exact_word, prefix_phrase];
    let r = recs(&[("大", 3, NOW)]); // 有词频记录 → 重排确实生效
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text,
        "大",
        "前缀短语(is_phrase && !is_exact_code)不锚定，须留在精确拼音候选之下，实际: {:?}",
        cands.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert_eq!(cands[1].text, "date短语");
}

/// 衰减软置前：近用词（衰减分 ≥ ε）浮到未用词之上，即使权重更低。
#[test]
fn pinyin_recent_use_floats_above_higher_weight() {
    let mut cands = vec![pin("低频高权", 5000), pin("近用低权", 100)];
    let r = recs(&[("近用低权", 8, NOW)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(cands[0].text, "近用低权", "近期使用应软置前");
}

fn pin_fuzzy(text: &str, weight: i32) -> Candidate {
    let mut c = pin(text, weight);
    c.is_fuzzy = true;
    c
}

/// 词频 used-first 不因「模糊来源」而豁免：用户在 "si" 下反复选过模糊命中「是」，
/// 词频重排应照常把它软置前——`is_fuzzy` 只是召回来源标记，不构成学习壁垒。
///
/// 别把 `is_fuzzy` 做回 `cmp_match_layers` 的首要层级键：真实词库下那会把模糊候选整体压到
/// 生产候选上限之外，模糊音等于未实现。模糊惩罚由引擎在 weight 上施加
/// （`pinyin::FUZZY_WEIGHT_SCALE`），进入本函数时权重已折过。
#[test]
fn pinyin_used_fuzzy_can_rank_above_exact() {
    let mut cands = vec![
        pin_fuzzy("是", 9000), // 模糊命中，且被频繁使用过
        pin("四", 100),        // 精确命中，未使用
    ];
    let r = recs(&[("是", 4, NOW)]); // 「是」有使用记录（模拟 si→是 count=4）
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        cands[0].text,
        "是",
        "用户反复选过的模糊命中应被词频软置前，实际: {:?}",
        cands.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert_eq!(cands[1].text, "四");
}

/// 阈值褪色：久未用（衰减分 < ε）失去 used-first 资格，落回引擎权重序。
#[test]
fn pinyin_faded_use_falls_back_to_weight_order() {
    let long_ago = NOW - 365 * 24 * 3600; // 一年前用过一次 → 衰减远小于 ε
    let mut cands = vec![pin("高权未用", 5000), pin("陈旧低权", 100)];
    let r = recs(&[("陈旧低权", 1, long_ago)]);
    rerank_pinyin_positional(
        &mut cands,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(cands[0].text, "高权未用", "褪色词应落回权重序，高权在前");
}

/// 码表 used-first（step）：用过的词按 count 降序置前，未用维持权重序殿后。
#[test]
fn codetable_step_orders_by_count() {
    let mut cands = vec![
        ct("aaaa", "工", 100),
        ct("aaaa", "戈", 90),
        ct("aaaa", "啊", 80),
    ];
    let r = recs(&[("戈", 5, NOW), ("工", 2, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "aaaa",
        FreqStrategy::Step,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "戈", "step：count 高者置前");
    assert_eq!(cands[1].text, "工");
    assert_eq!(cands[2].text, "啊", "未用词维持权重序殿后");
}

/// 码表 used-first（top/MRU）：最近用的置该档之首，与 count 无关。
#[test]
fn codetable_top_orders_by_recency() {
    let mut cands = vec![ct("aaaa", "工", 100), ct("aaaa", "戈", 90)];
    // 工 用 10 次但很久前；戈 仅 1 次但刚用
    let r = recs(&[("工", 10, NOW - 10_000), ("戈", 1, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "aaaa",
        FreqStrategy::Top,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "戈", "top：最近使用者置首");
}

/// 五笔优先档位：拼音候选即便高频近用，也不能浮到码表精确全码之上（混输硬约束）。
#[test]
fn mixed_tier_keeps_codetable_exact_above_pinyin() {
    let mut cands = vec![ct("aaaa", "工", 100), pin("啊", 5000)];
    // 拼音「啊」高频近用，但其档位低于码表精确全码档位 0
    let r = recs(&[("啊", 50, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "aaaa",
        FreqStrategy::Step,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "工", "码表精确全码档位最高，拼音不得反超");
}

/// 兜底档 `fallback=1`：重排后基础序首位被回填锁定，高词频候选在保护位之后正常上浮。
/// 三条候选的 `code` 均等于输入，都属精确档（保护名额只在该档内取，见
/// `protect_slots_taken_from_exact_only`）。
#[test]
fn protect_top_n_pins_original_head() {
    // 基础序：甲(高weight) 乙 丙；"丙"有词频记录本应浮首。
    let mut cands = vec![
        ct_exact("abcd", "甲", 300),
        ct_exact("abcd", "乙", 200),
        ct_exact("abcd", "丙", 100),
    ];
    let mut recs_map = HashMap::new();
    recs_map.insert(
        "丙".to_string(),
        FreqRecord {
            count: 9,
            last_used: 1000,
        },
    );
    rerank_codetable_usedfirst(
        &mut cands,
        &recs_map,
        "abcd",
        FreqStrategy::Step,
        ProtectPolicy {
            by_len: [0; 3],
            fallback: 1,
        },
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "甲", "protect_top_n=1 应锁定原首位");
    assert_eq!(cands[1].text, "丙", "词频候选在保护位之后正常上浮");
}

/// 精确档候选（`code == input`）。五笔简码字与全码字都属此档，是「词库钦定首选」的载体。
fn ct_exact(code: &str, text: &str, weight: i32) -> Candidate {
    let mut c = ct(code, text, weight);
    c.is_exact_code = true;
    c
}

/// 分级表按输入码长取值，码长 ≥ 4 落兜底档。
#[test]
fn protect_policy_resolve_by_len() {
    let p = ProtectPolicy {
        by_len: [3, 2, 1],
        fallback: 9,
    };
    assert_eq!(p.resolve("a"), 3, "一简位");
    assert_eq!(p.resolve("aa"), 2, "二简位");
    assert_eq!(p.resolve("aaa"), 1, "三简位");
    assert_eq!(p.resolve("aaaa"), 9, "全码位落兜底");
    assert_eq!(p.resolve("aaaaaa"), 9, "超长码同样落兜底");
    assert_eq!(p.resolve(""), 0, "空码不保护");
}

/// 主用例：一简位（码长 1）的词库钦定首选不被词频顶掉。
///
/// 取自 `wubi86_jidian.dict.yaml`：`a` → 工(9999) / 戈(9998)。一简每个码都是二选一，而本模块的
/// 比较链不含 weight，「戈」被误选一次即永久翻转（码表侧 used-first 不衰减）。
/// 必须与 `full_code_still_reranks_freely` 合看，否则「全局硬保护」的错误实现同样会绿。
#[test]
fn short_code_len1_protects_dict_head() {
    let mut cands = vec![ct_exact("a", "工", 9999), ct_exact("a", "戈", 9998)];
    let r = recs(&[("戈", 5, NOW)]); // 用户选过 5 次「戈」
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "a",
        FreqStrategy::Step,
        ProtectPolicy::default(),
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(
        cands[0].text,
        "工",
        "一简位钦定首选须恒居首，实际: {:?}",
        cands.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert_eq!(cands[1].text, "戈", "被保护的只是位次，戈仍在列表里");
}

/// 对照组：同一组数据放到全码位（码长 4）→ 词频照常生效。
/// 锁住「分级」本身：若把简码保护做成全局硬保护，本用例会红。
#[test]
fn full_code_still_reranks_freely() {
    let mut cands = vec![ct_exact("aaaa", "甲", 9999), ct_exact("aaaa", "乙", 9998)];
    let r = recs(&[("乙", 5, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "aaaa",
        FreqStrategy::Step,
        ProtectPolicy::default(),
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(
        cands[0].text,
        "乙",
        "全码位不设保护，用过的候选须正常上浮，实际: {:?}",
        cands.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 二简位同样受保护，且 `Top`（MRU）策略下结论一致——MRU 在简码位危害最大（选一次到顶）。
#[test]
fn short_code_len2_protects_under_mru() {
    let mut cands = vec![ct_exact("aa", "式", 9950), ct_exact("aa", "戒", 9949)];
    let r = recs(&[("戒", 1, NOW)]); // MRU 下一次即可到顶
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "aa",
        FreqStrategy::Top,
        ProtectPolicy::default(),
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "式", "二简位在 MRU 策略下同样须保住钦定首选");
}

/// 保护名额只在精确档内取：名额多于精确候选时，多出来的不落到前缀补全头上。
///
/// 打 `a`：精确档只有一简字「工」，其后全是 `a*` 的前缀补全。名额若按位置无差别取，补全词「工艺」
/// 会被钉死在第 2 位——它不是任何码位的钦定首选，且会挡住用户真正常用的「工区」。
#[test]
fn protect_slots_taken_from_exact_only() {
    let mut cands = vec![
        ct_exact("a", "工", 9999),
        ct("aaan", "工艺", 1717),
        ct("aaaq", "工区", 737),
    ];
    let r = recs(&[("工区", 5, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "a",
        FreqStrategy::Step,
        ProtectPolicy {
            by_len: [2, 0, 0], // 名额 2 > 精确候选数 1
            fallback: 0,
        },
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    let order: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        order,
        vec!["工", "工区", "工艺"],
        "名额只在精确档内取：工受保护，补全档内部照常按词频重排"
    );
}

/// 该码位没有精确候选（打了词库里没有的码，候选全是前缀补全）→ 保护集为空。
/// 没有钦定首选可言，不该平白钉死一个补全词。
#[test]
fn no_exact_candidate_means_no_protection() {
    let mut cands = vec![ct("aaan", "工艺", 1717), ct("aaaq", "工区", 737)];
    let r = recs(&[("工区", 5, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "a",
        FreqStrategy::Step,
        ProtectPolicy {
            by_len: [1, 0, 0],
            fallback: 0,
        },
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "工区", "无精确候选时不设保护，词频照常生效");
}

/// 空策略退化为「无保护」，与分级引入前逐字节一致。
#[test]
fn none_policy_degrades_to_no_protection() {
    let mut cands = vec![ct_exact("a", "工", 9999), ct_exact("a", "戈", 9998)];
    let r = recs(&[("戈", 5, NOW)]);
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "a",
        FreqStrategy::Step,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(cands[0].text, "戈", "NONE 策略下词频照常生效");
}

/// freq_tier 短语再分档：精确码短语（is_exact_code）与码表精确同档（tier 0）；前缀短语
/// （!is_exact_code）排在码表前缀补全之后，不因 is_phrase 抬到补全之上。
///
/// 混输 `da`：入参按 `candidate_display_order` 的输出序喂入（精确码 → 精确码短语 → 码表前缀补全
/// → 前缀短语），验证 rerank 维持该档位结构；`is_phrase` 一刀切进高档时前缀短语会排到「矼」之前。
#[test]
fn codetable_tier_prefix_phrase_stays_with_completion() {
    let exact_code = ct("da", "左", 3000); // 码表精确全码 tier 0
    let exact_phrase = {
        let mut c = ct("", "精确短语", 10); // lookup 精确码短语，与上者同 tier 0
        c.is_phrase = true;
        c.is_exact_code = true;
        c
    };
    let completion = ct("dax", "矼", 9000); // 码表前缀补全 tier 2（真实词频，加成已拆除）
    let prefix_phrase = {
        let mut c = ct("", "date短语", 10);
        c.is_phrase = true;
        c.is_prefix = true; // is_exact_code 默认 false
        c
    }; // lookup_prefix 前缀短语 tier 3
    let mut cands = vec![exact_code, exact_phrase, completion, prefix_phrase];
    let r = recs(&[]); // 无词频记录 → 纯按档位 + 稳定序（维持入参显示序）
    rerank_codetable_usedfirst(
        &mut cands,
        &r,
        "da",
        FreqStrategy::Step,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    let order: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        order,
        vec!["左", "精确短语", "矼", "date短语"],
        "精确码短语与码表精确同 tier 0（维持入参序）；前缀短语 tier3 排在码表前缀补全 tier2 之后"
    );
}

/// 开调频不得推翻协调器按权重排出的「短语 vs 码表精确」次序。
///
/// 这是把精确码短语并入 tier 0 的唯一动因：协调器在纯码表下按权重比二者，而 `freq_tier` 是
/// `rerank_codetable_usedfirst` 的首要键、整体压过显示序。若分档，同一个输入就会「开调频码表精确
/// 恒赢、关调频按权重比」，开关（`[schema.codetable.frequency] enabled`，默认关）一开短语行为就变。
///
/// 两个方向都测：只测「短语在前」时，断言方向恰与「码表恒赢」一致就证明不了合并，能证伪的是第 ① 条。
/// 无词频记录 ⇒ 档内稳定排序维持入参序：调频重排的比较链不含 weight（见 frequency.md §8）。
#[test]
fn codetable_tier_exact_phrase_keeps_coordinator_weight_order() {
    let phrase = |w: i32| {
        let mut c = ct("", "短语", w);
        c.is_phrase = true;
        c.is_exact_code = true;
        c
    };
    let exact = |w: i32| ct("skce", "可能", w);
    let run = |mut cands: Vec<Candidate>| -> Vec<String> {
        rerank_codetable_usedfirst(
            &mut cands,
            &recs(&[]),
            "skce",
            FreqStrategy::Step,
            ProtectPolicy::NONE,
            NOW,
            FreqProfile::default(),
            PromotePrefix::All,
        );
        cands.into_iter().map(|c| c.text).collect()
    };
    // ① 短语权重更高 → 协调器把它排在前，本模块须维持。
    //    旧码（精确码短语独占 tier 1）会把「可能」提到最前 ⇒ 本断言变红。
    assert_eq!(
        run(vec![phrase(1800), exact(500)]),
        vec!["短语", "可能"],
        "短语 1800 > 码表 500：开调频不得把码表精确提到短语之前"
    );
    // ② 反向对照：码表权重更高时次序同样维持，证明不是「短语恒前」。
    assert_eq!(
        run(vec![exact(3000), phrase(1800)]),
        vec!["可能", "短语"],
        "码表 3000 > 短语 1800：次序同样由入参（协调器权重序）决定"
    );
}

// ─────────────── 位置提升（docs/design/freq-rerank-model.md）───────────────
//
// 下列常量是 `build_dev/data/schemas/pinyin/cn_dicts/` 的实测权重，用来构造真实的位次关系——
// 位置模型本身不看权重，但入参顺序必须与生产一致（按显示序排好）。

/// `de` 音单字：的 > 得 > 地 > 德，「的」是第二名的 486 倍。
/// 位置模型下这个悬殊比例不影响提升速度，这正是它取代权重模型的理由。
const W_DE: i32 = 15_378_475;
const W_DEI: i32 = 31_646;
const W_DI: i32 = 13_039;
/// `si yuan`：寺院 491 > 思源 245，仅 2 倍。
const W_SIYUAN_TEMPLE: i32 = 491;
const W_SIYUAN_PRODUCT: i32 = 245;

/// 有同码竞争者的整句。weight 取「寺院」的真实词典值 491 —— 同文合并取
/// `max(词典 weight, W_eff)`，整句退役 `SENTENCE_WEIGHT_BASE` 后这就是它的实际量级。
/// 位置模型不看这个数值，它只决定 base_pos。
fn pin_contested(text: &str) -> Candidate {
    let mut c = pin(text, W_SIYUAN_TEMPLE);
    c.is_sentence = true;
    c
}

/// `count=1` 的记录在经过一段时间后仍须生效。
///
/// `promotion_power` 不能向零截断（`(count * decay) as u32`）：选完 1 分钟 decay 就小于 1，
/// 截断得 0，提升完全消失。关键在于 `last_used != now`：涉及衰减的用例必须让时间真的流逝，
/// `now == last_used` 时 decay 恰为 1.0，正好落在唯一能通过的点上。
#[test]
fn single_use_still_promotes_after_realistic_delay() {
    for (label, secs) in [("1 分钟", 60i64), ("1 小时", 3600), ("1 天", 86_400)] {
        let mut c = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
        let r = recs(&[("思源", 1, NOW - secs)]);
        rerank_pinyin_positional(
            &mut c,
            &r,
            NOW,
            FreqProfile::default(),
            PromotePrefix::All,
            0,
        );
        assert_eq!(
            c[0].text, "思源",
            "选过一次、{label}后再打，仍应居首（decay 略小于 1 不得被截断成 0）"
        );
    }
}

/// 配对：衰减够久之后必须失效，否则上面那条可以靠「永不衰减」满足。
/// 半衰期 72h，`MIN_PROMOTION_POWER = 0.5` ⇒ `count=1` 的记录约一个半衰期后归零。
#[test]
fn single_use_expires_after_long_disuse() {
    let mut c = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
    // 30 天 ≈ 10 个半衰期，decay ≈ 0.001
    let r = recs(&[("思源", 1, NOW - 30 * 86_400)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        c[0].text, "寺院",
        "一个月前用过一次不应再影响排序——否则位次的 floor 会让任何残余强度都置顶第 2 位"
    );
}

/// 高频记录的衰减边界——含一条已知局限。
///
/// 半衰期 72h：一周后 `count=50` 的有效强度仍有 9.9；30 天（≈10 个半衰期）后衰减到 0.049，
/// 低于 `MIN_PROMOTION_POWER` 而归零——用过 50 次的词，一个月不用就被完全遗忘。
///
/// 这是墙钟衰减的固有行为（fcitx5 用「被后续输入挤出分级 LRU 池」避免它），换老化机制需改
/// 词频存储模型，另行立项。本用例钉死当前行为，换机制时连同本注释一起更新。
#[test]
fn frequent_use_decay_boundary_including_known_limitation() {
    // 一周后仍生效：50 × decay(168h)=0.198 → 9.9
    let mut week = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
    let r_week = recs(&[("思源", 50, NOW - 7 * 86_400)]);
    rerank_pinyin_positional(
        &mut week,
        &r_week,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(week[0].text, "思源", "用过 50 次的词一周后仍应保持提升");

    // 30 天后归零 —— 已知局限
    let mut month = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
    let r_month = recs(&[("思源", 50, NOW - 30 * 86_400)]);
    rerank_pinyin_positional(
        &mut month,
        &r_month,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        month[0].text, "寺院",
        "墙钟衰减下高频词一个月后也会被遗忘——已知局限，换分级 LRU 老化可解"
    );
}

/// 首位候选自身有词频时，不得被后面的提升者挤下去（`siyuan` 下「寺院」`base_pos=0`、`p=8.0`）。
///
/// 排序键第四项的判据是「有没有词频」，不能写成 `target < base_pos`（「是否真的前移」）：
/// `base_pos=0` 无处可移，会恒判为「未提升」而排到所有提升者之后。
#[test]
fn top_candidate_with_freq_is_not_pushed_down() {
    let mut c = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
    let r = recs(&[("寺院", 8, NOW), ("思源", 6, NOW)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        c[0].text, "寺院",
        "首位候选用得更多，不该因为「没法再往前」而掉位"
    );
}

/// 反向对照：首位不是被钉死的——竞争者用得更多时仍能反超。
/// 缺了它，上面那条可以靠「base_pos=0 恒居首」满足。
#[test]
fn top_candidate_yields_to_stronger_peer() {
    let mut c = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
    let r = recs(&[("寺院", 3, NOW), ("思源", 10, NOW)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(
        c[0].text, "思源",
        "同目标位次时按有效强度排序，用得多的在前"
    );
}

/// `promote_prefix = false`（拼音默认）：前缀补全候选不参与位置提升。
///
/// 打 `d` 时「东西」（`dongxi`）连选几次就会从第 40 位爬到首位，压过「的」「得」；短输入下信息量
/// 很少，把长词组顶到单字前面与直觉相悖（微软拼音也只对全码生效）。
#[test]
fn prefix_completion_not_promoted_when_switch_off() {
    let completion = |t: &str, w: i32| {
        let mut c = pin(t, w);
        c.is_prefix = true;
        c
    };
    // 打 `d` 时所有候选都是前缀补全，层级相同，位次才说了算。若把「的」「得」构造成精确匹配，
    // `cmp_match_layers` 会先把「东西」降层，本用例就测不到开关本身了。
    let build = || {
        vec![
            completion("的", W_DE),
            completion("得", W_DEI),
            completion("东西", 90_000),
        ]
    };
    let r = recs(&[("东西", 6, NOW)]);

    let mut off = build();
    rerank_pinyin_positional(
        &mut off,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::None,
        0,
    );
    assert_eq!(
        off.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["的", "得", "东西"],
        "开关关闭时前缀补全不得因词频前移"
    );

    // 对照：开启后同样的记录能把它提到首位——否则本用例只是在测「提升从未工作」
    let mut on = build();
    rerank_pinyin_positional(
        &mut on,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(on[0].text, "东西", "开关开启时应恢复提升");
}

/// 开关只挡有效前缀层：被引擎提升进完整匹配层的候选（残码上浮 / 用户长词上浮）
/// 仍可调频——那是结构决策，与「码更长」无关。
#[test]
fn promoted_completion_still_reranks_when_switch_off() {
    let mut promoted = pin("没有", 5000);
    promoted.is_prefix = true;
    promoted.is_promoted_completion = true;
    let mut c = vec![pin("每", 90_000), promoted];
    let r = recs(&[("没有", 1, NOW)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::None,
        0,
    );
    assert_eq!(
        c[0].text, "没有",
        "已提升进完整匹配层的候选不受 promote_prefix 影响"
    );
}

/// `Single` 档（默认）：单字提升、词组不提升——短输入下的关键区分。
/// 打 `d` 时所有候选都是前缀补全，`Single` 只挡词组，不把「的/得」之间的正常调整一并挡死。
#[test]
fn single_tier_promotes_chars_but_not_words() {
    let completion = |t: &str, w: i32| {
        let mut c = pin(t, w);
        c.is_prefix = true;
        c
    };
    // 单字「得」应能提升
    let mut chars = vec![completion("的", W_DE), completion("得", W_DEI)];
    rerank_pinyin_positional(
        &mut chars,
        &recs(&[("得", 1, NOW)]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::Single,
        0,
    );
    assert_eq!(chars[0].text, "得", "Single 档下单字补全应可提升");

    // 词组「东西」不应提升
    let mut words = vec![
        completion("的", W_DE),
        completion("得", W_DEI),
        completion("东西", 90_000),
    ];
    rerank_pinyin_positional(
        &mut words,
        &recs(&[("东西", 6, NOW)]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::Single,
        0,
    );
    assert_eq!(
        words.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["的", "得", "东西"],
        "Single 档下 2 个语义单元的词组不得提升"
    );
}

/// 英文必须能调频：`Single` 档按语义单元数判定，英文词整体计 1。
///
/// 英文候选几乎都是前缀匹配（`hel` 出 `hello`/`help`），判据若用 `chars().count()`，
/// `hello` = 5 会被 `Single` 挡死，英文调频全灭。
#[test]
fn english_words_still_promote_in_single_tier() {
    let completion = |t: &str, w: i32| {
        let mut c = pin(t, w);
        c.is_prefix = true;
        c
    };
    let mut c = vec![
        completion("help", 9000),
        completion("hello", 5000),
        completion("hell", 3000),
    ];
    rerank_pinyin_positional(
        &mut c,
        &recs(&[("hello", 1, NOW)]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::Single,
        0,
    );
    assert_eq!(
        c[0].text, "hello",
        "英文词是 1 个语义单元，Single 档下应可提升"
    );

    // 对照：带空格的英文词组是 2 个单元，同档下不提升
    let mut phrase = vec![completion("thanks", 9000), completion("thank you", 5000)];
    rerank_pinyin_positional(
        &mut phrase,
        &recs(&[("thank you", 6, NOW)]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::Single,
        0,
    );
    assert_eq!(
        phrase[0].text, "thanks",
        "「thank you」是 2 个语义单元，与中文词组同等对待"
    );
}

// ─────────────── 码表 / 英文的 position 策略 ───────────────

/// 英文方案的典型场景：候选几乎全是前缀匹配，布尔 used-first 过于粗暴。
/// `Top`/`Step` 下选一次 `hello` 就整体跳到档内最前；`Position` 则逐次前移。
#[test]
fn codetable_position_promotes_english_gradually() {
    let en = |code: &str, t: &str, w: i32| {
        let mut c = ct(code, t, w);
        c.source = CandidateSource::English;
        c
    };
    let build = || {
        vec![
            en("hel", "help", 9000),
            en("hel", "hello", 5000),
            en("hel", "hell", 3000),
            en("hel", "helmet", 1000),
        ]
    };

    // 用一次：第 1 位 → 第 0 位
    let mut once = build();
    rerank_codetable_usedfirst(
        &mut once,
        &recs(&[("hello", 1, NOW)]),
        "hel",
        FreqStrategy::Position,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(once[0].text, "hello", "第 2 位用一次即到首位");

    // 末位候选需要更多次才爬到前面——这就是「渐进」相对布尔闸门的差别
    let mut tail = build();
    rerank_codetable_usedfirst(
        &mut tail,
        &recs(&[("helmet", 1, NOW)]),
        "hel",
        FreqStrategy::Position,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(
        tail.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["help", "helmet", "hello", "hell"],
        "第 4 位用一次只到第 2 位（3/2=1），不是直接置顶"
    );
}

/// 对照：同样的记录在 `Step` 下会直接跳到档内最前。
/// 缺了它，上面那条无从证明 `Position` 确实换了行为。
#[test]
fn codetable_step_is_boolean_gate_by_contrast() {
    let en = |code: &str, t: &str, w: i32| {
        let mut c = ct(code, t, w);
        c.source = CandidateSource::English;
        c
    };
    let mut c = vec![
        en("hel", "help", 9000),
        en("hel", "hello", 5000),
        en("hel", "hell", 3000),
        en("hel", "helmet", 1000),
    ];
    rerank_codetable_usedfirst(
        &mut c,
        &recs(&[("helmet", 1, NOW)]),
        "hel",
        FreqStrategy::Step,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(
        c[0].text, "helmet",
        "Step 是布尔闸门：末位候选用一次即整体跳到最前"
    );
}

/// `Position` 下来源档位仍是硬约束：前缀补全再怎么提升也跨不到精确全码之前。
/// 英文场景要的是档内提升，不是跨档。
#[test]
fn codetable_position_respects_source_tiers() {
    let mut c = vec![
        ct("a", "工", 9999),   // tier 0：精确全码
        ct("ab", "式", 5000),  // tier 2：前缀补全
        ct("abc", "戒", 3000), // tier 2
    ];
    rerank_codetable_usedfirst(
        &mut c,
        &recs(&[("戒", 50, NOW)]),
        "a",
        FreqStrategy::Position,
        ProtectPolicy::NONE,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
    );
    assert_eq!(
        c[0].text, "工",
        "精确全码恒在前，前缀补全用 50 次也跨不过档位"
    );
    assert_eq!(c[1].text, "戒", "但档内提升生效：它越过了同档的「式」");
}

/// 模型的核心断言：提升速度与权重差距无关。
///
/// `de` 下「的」是第二名的 486 倍，`siyuan` 下「寺院」只是「思源」的 2 倍，两处的第 2 位候选都应
/// 用一次到首位。权重模型的同一套参数在两种落差下必然一处太慢、一处太快。
#[test]
fn promotion_speed_is_independent_of_weight_gap() {
    // ① 486 倍落差
    let mut de = vec![pin("的", W_DE), pin("得", W_DEI), pin("地", W_DI)];
    let r = recs(&[("得", 1, NOW)]);
    rerank_pinyin_positional(
        &mut de,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(de[0].text, "得", "第 2 位用一次即到首位（486 倍落差）");

    // ② 2 倍落差——同样一次
    let mut sy = vec![pin("寺院", W_SIYUAN_TEMPLE), pin("思源", W_SIYUAN_PRODUCT)];
    let r2 = recs(&[("思源", 1, NOW)]);
    rerank_pinyin_positional(
        &mut sy,
        &r2,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(sy[0].text, "思源", "第 2 位用一次即到首位（2 倍落差）");
}

/// 位次逐次减半：第 8 位 → 4 → 2 → 1 → 0。
/// 对应微软拼音的行为「1 次提前一半、2 次进第 2 位、再 1 次到首位」。
#[test]
fn position_halves_on_each_use() {
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    // 权重递减，保证入参即显示序；目标候选 "i" 在第 8 位（idx 8）
    let build = || -> Vec<Candidate> {
        names
            .iter()
            .enumerate()
            .map(|(i, t)| pin(t, 1000 - i as i32))
            .collect()
    };
    for (count, want) in [(1u32, 4usize), (2, 2), (3, 1), (4, 0)] {
        let mut c = build();
        let r = recs(&[("i", count, NOW)]);
        rerank_pinyin_positional(
            &mut c,
            &r,
            NOW,
            FreqProfile::default(),
            PromotePrefix::All,
            0,
        );
        let got = c.iter().position(|x| x.text == "i").unwrap();
        assert_eq!(got, want, "用 {count} 次后应在第 {want} 位，实际 {got}");
    }
}

/// 反向对照：没有词频记录就一步都不动。
/// 缺了它，「无条件把某些候选前移」也能让上面两条通过。
#[test]
fn no_record_means_no_movement() {
    let mut c = vec![pin("的", W_DE), pin("得", W_DEI), pin("地", W_DI)];
    let before: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
    rerank_pinyin_positional(
        &mut c,
        &recs(&[]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    let after: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
    assert_eq!(before, after, "无词频记录时顺序必须原样保持");
}

/// contested 整句可被同码竞争者靠位置提升反超（`siyuan` 寺院/思源）。
///
/// 位置模型下不需要 `dict_weight`：整句的权重只决定它的 `base_pos=0`，
/// 竞争者提升到同一目标位次后靠「被提升者在前」胜出。
#[test]
fn contested_sentence_can_be_overtaken_by_promoted_peer() {
    let mut c = vec![pin_contested("寺院"), pin("思源", W_SIYUAN_PRODUCT)];
    let r = recs(&[("思源", 1, NOW)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(c[0].text, "思源", "选过一次的同码词应反超 contested 整句");
    assert_eq!(c[1].text, "寺院", "整句退居第二，不是被赶出列表");
}

/// 反向对照：无词频记录时 contested 整句仍居首，不得平白掉位。
#[test]
fn contested_sentence_still_leads_without_record() {
    let mut c = vec![pin_contested("寺院"), pin("思源", W_SIYUAN_PRODUCT)];
    rerank_pinyin_positional(
        &mut c,
        &recs(&[]),
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(c[0].text, "寺院", "无词频记录时整句解仍是最优解读");
}

/// 锚定候选不参与位置提升：用再多次也推不动它。
/// 锚定的唯一享用者是精确码短语（用户显式定义、码完全匹配）；整句不锚定，见
/// `pinyin_sentence_can_be_overtaken_by_frequent_peer`。
#[test]
fn anchored_exact_phrase_is_immune_to_promotion() {
    let phrase = {
        let mut c = pin("我的邮箱", 5000);
        c.is_phrase = true;
        c.is_exact_code = true;
        c
    };
    let mut c = vec![phrase, pin("普通词", 5000)];
    let r = recs(&[("普通词", 50, NOW)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(c[0].text, "我的邮箱", "锚定的精确码短语不受位置提升影响");
}

/// 衰减：久未用则有效次数归零，位次回落原处。
#[test]
fn promotion_decays_with_time() {
    let long_ago = NOW - 365 * 24 * 3600;
    let mut c = vec![pin("的", W_DE), pin("得", W_DEI), pin("地", W_DI)];
    let r = recs(&[("得", 1, long_ago)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(c[0].text, "的", "一年前用过一次 → 衰减殆尽 → 不再提升");

    // 对照：同样的记录、刚用过则生效——否则上面可能只是「提升从未工作」
    let mut fresh = vec![pin("的", W_DE), pin("得", W_DEI), pin("地", W_DI)];
    let r2 = recs(&[("得", 1, NOW)]);
    rerank_pinyin_positional(
        &mut fresh,
        &r2,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(fresh[0].text, "得", "刚用过必须生效");
}

/// 位置提升不得跨匹配层级：前缀补全再常用也压不过精确候选。
///
/// 必须同时钉住「层内提升确实生效」：只断言「补全没上来」的话，「提升机制整个失效」也能通过
/// （令 `promotion_steps` 恒 0 时其余用例全红，唯独只断言一半的版本还绿）。
#[test]
fn promotion_does_not_cross_match_layers() {
    let completion = |t: &str, w: i32| {
        let mut c = pin(t, w);
        c.is_prefix = true;
        c
    };
    // 入参顺序即显示序：精确「四」在前，两条补全按权重降序
    let mut c = vec![
        pin("四", 100),
        completion("思考", 900_000),
        completion("思路", 800_000),
    ];
    let r = recs(&[("思路", 50, NOW)]);
    rerank_pinyin_positional(
        &mut c,
        &r,
        NOW,
        FreqProfile::default(),
        PromotePrefix::All,
        0,
    );
    assert_eq!(c[0].text, "四", "补全用 50 次也不得跨层压过精确候选");
    assert_eq!(
        c[1].text, "思路",
        "但层内提升必须生效——否则本用例测的是「提升从未工作」"
    );
}
