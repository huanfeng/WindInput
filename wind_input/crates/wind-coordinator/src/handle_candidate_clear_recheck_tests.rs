//! 满码空码清空的第三道门（`clear_blocked_by_candidates`）：哪些候选拦得住清空。
//!
//! 前两道在引擎内（码表 `clear_on_empty_max` → 混输 `should_clear` 的拼音守护），
//! 见 `wind-engine` 的 `mixed_trailing_partial_pinyin` 与 `mixed::engine::tests`。
use super::*;

fn pinyin(text: &str, consumed: usize) -> Candidate {
    Candidate {
        text: text.into(),
        source: CandidateSource::Pinyin,
        consumed_length: consumed,
        ..Default::default()
    }
}

fn codetable(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        source: CandidateSource::CodeTable,
        // 码表候选恒不标注消费长度（选词即消费整串）。
        consumed_length: 0,
        ..Default::default()
    }
}

#[test]
fn codetable_split_shows_only_on_highlighted_sentence() {
    let sentence = Candidate {
        text: "工作工作".into(),
        source: CandidateSource::CodeTable,
        is_sentence: true,
        ..Default::default()
    };
    let plain = codetable("工作");

    assert!(
        wants_codetable_split("aawt'aawt", Some(&sentence)),
        "高亮整句候选时应显示切分"
    );
    assert!(
        !wants_codetable_split("aawt'aawt", Some(&plain)),
        "高亮普通码表候选时不得显示切分——那个切法不对应它"
    );
    assert!(
        !wants_codetable_split("", Some(&sentence)),
        "没有切分串时不显示"
    );
    assert!(!wants_codetable_split("aawt'aawt", None), "无候选时不显示");
    // 拼音整句不走这条：它有自己的音节拆分形态。
    let pinyin_sentence = Candidate {
        text: "工作".into(),
        source: CandidateSource::Pinyin,
        is_sentence: true,
        ..Default::default()
    };
    assert!(!wants_codetable_split("aawt'aawt", Some(&pinyin_sentence)));
}

/// 逆切分候选走同一条组合区判据（同一出口 `preedit_codetable`、同一分隔符），
/// 身份标记是它自己的 `is_split_composed`。
///
/// 反向对照不能省：只断言正向时，把判据写成无条件 true 的实现也会变绿。
#[test]
fn codetable_split_shows_on_highlighted_split_composition() {
    let composed = Candidate {
        text: "很可能".into(),
        source: CandidateSource::CodeTable,
        is_split_composed: true,
        ..Default::default()
    };
    assert!(
        wants_codetable_split("hf'kn", Some(&composed)),
        "高亮逆切分候选时应显示 hf'kn"
    );
    assert!(
        !wants_codetable_split("hf'kn", Some(&codetable("很可"))),
        "高亮普通码表候选时不得显示切分"
    );
    // 非码表来源即便带标记也不走这条（同拼音整句那一条的理由）。
    let alien = Candidate {
        text: "很可能".into(),
        source: CandidateSource::Pinyin,
        is_split_composed: true,
        ..Default::default()
    };
    assert!(!wants_codetable_split("hf'kn", Some(&alien)));
}

/// 草稿候选沉在一切真词之后，且比切分候选还靠后。
///
/// 草稿杂词率高，无论权重如何都不能挤到用户打得出的候选前面。
/// 见 `docs/design/auto-phrase-draft-layer.md` §5。
#[test]
fn draft_sinks_below_everything_including_split_composed() {
    let mut ordinary = codetable("甲");
    ordinary.weight = 10; // 故意给一个低权重
    // 同逆切分那条：必须带 `is_prefix`，否则两条候选在 `cmp_match_layers` 的
    // `eff_prefix` 层就分出胜负，沉底层放在它之后也能变绿——那是假护栏。
    ordinary.is_prefix = true;
    let mut draft = codetable("甲乙丙");
    draft.is_draft = true;
    draft.weight = 9999; // 故意给一个高权重

    assert_eq!(
        candidate_display_order(&ordinary, &draft, false, false, "abcd"),
        std::cmp::Ordering::Less,
        "权重再高的草稿也须排在普通候选之后"
    );

    // 比切分候选还靠后：判据是「谁更未经验证」。
    let mut composed = codetable("很可能");
    composed.is_split_composed = true;
    composed.weight = 1;
    assert_eq!(
        candidate_display_order(&composed, &draft, false, false, "abcd"),
        std::cmp::Ordering::Less,
        "草稿该沉在切分候选之后——切分是按码表规则拼的，草稿只是滑窗猜的"
    );

    // 反向对照：两条都不是草稿时，本层必须是空操作，权重照常说了算。
    // 缺了它，一个「无条件把 a 排前」的实现也能让上面两句变绿。
    let mut other = codetable("乙");
    other.weight = 9999;
    assert_eq!(
        candidate_display_order(&ordinary, &other, false, false, "abcd"),
        std::cmp::Ordering::Greater,
        "两条普通候选之间，沉底层不得改变既有次序"
    );

    // 两条都是草稿时同样是空操作，序交给后续键（weight）。
    let mut draft_low = draft.clone();
    draft_low.text = "甲乙丁".into();
    draft_low.weight = 1;
    assert_eq!(
        candidate_display_order(&draft, &draft_low, false, false, "abcd"),
        std::cmp::Ordering::Less,
        "草稿之间仍按权重排"
    );
}

/// 沉底须在真实的整表重排（`build_candidates` 的 `sort_by(candidate_display_order(..))`）下成立；
/// 只断言两两比较，测不出「比较器不构成全序、`sort_by` 结果未指定」。
#[test]
fn drafts_land_at_the_tail_after_a_full_resort() {
    let mut list = Vec::new();
    for (text, weight, is_draft) in [
        ("草稿甲", 9999, true),
        ("真词一", 5, false),
        ("草稿乙", 8888, true),
        ("真词二", 3000, false),
        ("真词三", 1, false),
    ] {
        let mut c = codetable(text);
        c.weight = weight;
        c.is_draft = is_draft;
        // 真词带 `is_prefix`、草稿不带：两者在 `eff_prefix` 层相同时，
        // 沉底键错放到那一层之后也会变绿。
        c.is_prefix = !is_draft;
        list.push(c);
    }
    list.sort_by(|a, b| candidate_display_order(a, b, false, false, "abcd"));
    let texts: Vec<&str> = list.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        &texts[..3],
        &["真词二", "真词一", "真词三"],
        "真词按权重降序（3000 → 5 → 1）排在前面：{texts:?}"
    );
    assert!(
        texts[3].starts_with("草稿") && texts[4].starts_with("草稿"),
        "两条草稿必须落在整表末尾，与权重无关：{texts:?}"
    );
}

/// `split_trigger = "no_exact"` 档的前提：切分候选沉在既有候选之后。
///
/// 引擎侧 append 只保证入列序，本函数会重排全部候选；少了沉底，
/// 高权重的组合候选会插进用户本来打得出的候选中间。
#[test]
fn split_composed_sinks_below_ordinary_candidates() {
    let mut ordinary = codetable("甲");
    ordinary.weight = 10; // 故意给一个低权重
    // 必须带 `is_prefix`（用户词/临时词层前缀补全的形态）：留 false 时两条在
    // `eff_prefix` 层相等，沉底层放错位置也能变绿。
    ordinary.is_prefix = true;
    let mut composed = codetable("很可能");
    composed.is_split_composed = true;
    composed.weight = 9999; // 故意给一个高权重

    assert_eq!(
        candidate_display_order(&ordinary, &composed, false, false, "hfkn"),
        std::cmp::Ordering::Less,
        "权重再高的切分候选也须排在普通候选之后（沉底层先于 weight 生效）"
    );

    // 反向对照：两条都不是切分候选时，本层必须是空操作，权重照常说了算。
    // 缺了它，一个「无条件把 a 排前」的实现也能让上面那句变绿。
    let mut other = codetable("乙");
    other.weight = 9999;
    assert_eq!(
        candidate_display_order(&ordinary, &other, false, false, "hfkn"),
        std::cmp::Ordering::Greater,
        "两条普通候选之间，沉底层不得改变既有次序（高权重者仍在前）"
    );

    // 两条都是切分候选时同样是空操作，序交给后续键（weight）。
    let mut composed_low = composed.clone();
    composed_low.text = "很可难".into();
    composed_low.weight = 1;
    assert_eq!(
        candidate_display_order(&composed, &composed_low, false, false, "hfkn"),
        std::cmp::Ordering::Less,
        "切分候选之间按权重，本层不得干预"
    );
}

#[test]
fn empty_list_never_blocks() {
    assert!(!clear_blocked_by_candidates(&[], 4), "空列表不得拦清空");
}

/// 真机现象 `nunl`：拼音候选「嫩」只解释了前 3 码 `nun`，没解释完整串 → 不算匹配。
/// 这是修复前 `state.candidates.is_empty()` 那版的直接失效点。
#[test]
fn partial_pinyin_candidate_does_not_block() {
    let cands = vec![pinyin("嫩", 3)];
    assert!(
        !clear_blocked_by_candidates(&cands, 4),
        "拼音部分匹配（consumed 3 < 4）不得替整串挡下清空"
    );
}

/// 反向锁：消费整串的拼音候选（`nuan`→「暖」，码表无字）是货真价实的匹配，必须拦住——
/// 否则关掉守护开关的用户再也打不出那些只有拼音出得来的字。
#[test]
fn full_pinyin_candidate_blocks() {
    let cands = vec![pinyin("暖", 4)];
    assert!(
        clear_blocked_by_candidates(&cands, 4),
        "拼音消费整串 → 有效匹配，必须拦住清空"
    );
}

/// 非拼音来源（短语 / 码表 / 英文）一律拦住：引擎算 `should_clear` 时看不见协调器
/// 随后追加的短语，本道门存在的原始理由就是它。
#[test]
fn non_pinyin_candidate_always_blocks() {
    assert!(
        clear_blocked_by_candidates(&[codetable("工")], 4),
        "码表候选（consumed_length=0 未标注）须视为整串匹配并拦住清空"
    );
    let phrase = Candidate {
        text: "地址".into(),
        is_phrase: true,
        ..Default::default()
    };
    assert!(
        clear_blocked_by_candidates(&[phrase], 4),
        "短语候选必须拦住清空（zzbd 等码表无字但短语命中）"
    );
}

/// 混合列表：只要有一条有效候选就拦住，部分匹配的拼音不参与计数。
#[test]
fn mixed_list_blocks_only_on_effective_candidate() {
    let only_partial = vec![pinyin("嫩", 3), pinyin("女", 2)];
    assert!(
        !clear_blocked_by_candidates(&only_partial, 4),
        "全是拼音部分匹配 → 不拦"
    );
    let with_full = vec![pinyin("嫩", 3), pinyin("暖", 4)];
    assert!(
        clear_blocked_by_candidates(&with_full, 4),
        "混有一条消费整串的拼音候选 → 拦住"
    );
}
