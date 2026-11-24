use super::*;

/// `consumed`：0 = 未标注（按整串算），与 `is_pinyin_exact_tier` 同约定。
fn zh(text: &str, code: &str, common: bool, consumed: usize) -> Candidate {
    Candidate {
        text: text.into(),
        code: code.into(),
        source: CandidateSource::Pinyin,
        is_common: common,
        consumed_length: consumed,
        ..Default::default()
    }
}

fn en(text: &str, code: &str) -> Candidate {
    Candidate {
        text: text.into(),
        code: code.into(),
        source: CandidateSource::English,
        ..Default::default()
    }
}

fn texts(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|x| x.text.as_str()).collect()
}

/// `hen`：很/恨/狠/痕 是常用精确解，佷/𬣳 是生僻字。英文插在常用段之后、生僻字之前，
/// 不能靠 `is_exact_code`（码表域标志）在 `cmp_exact_first` 压过全部中文。
#[test]
fn english_lands_after_common_exact_run() {
    let mut v = vec![
        zh("很", "hen", true, 3),
        zh("恨", "hen", true, 3),
        zh("狠", "hen", true, 3),
        zh("痕", "hen", true, 3),
        zh("佷", "hen", false, 3),
        zh("𬣳", "hen", false, 3),
        en("hen", "hen"),
    ];
    place_english_after_common_exact(&mut v, "hen", (1, 1), None);
    assert_eq!(
        texts(&v),
        vec!["很", "恨", "狠", "痕", "hen", "佷", "𬣳"],
        "英文应紧跟在开头那段常用精确解之后"
    );
}

/// `hello`：中文侧只有 Viterbi 整句「和理论哦」消费整串，其余单字不在精确档，英文排第 2。
///
/// 钉住「中文解得差时英文自动上前」，不靠权重阈值。
#[test]
fn english_second_when_chinese_side_is_thin() {
    let mut v = vec![
        Candidate {
            is_sentence: true,
            is_abbrev: true,
            ..zh("和理论哦", "hello", true, 5)
        },
        zh("和", "he", true, 2),
        zh("喝", "he", true, 2),
        en("hello", "hello"),
    ];
    place_english_after_common_exact(&mut v, "hello", (1, 1), None);
    assert_eq!(texts(&v), vec!["和理论哦", "hello", "和", "喝"]);
}

/// 回归：五笔打 `git` / `vim` / `npm`（中文侧只有前缀补全、没有完整匹配）时，出厂
/// `(exact 1, complete 2)` 下完整单词英文排第 1。
#[test]
fn exact_english_first_when_no_full_match_chinese_by_default() {
    let mut v = vec![
        zh("工", "g", true, 1),
        zh("国", "gi", true, 2),
        en("git", "git"),
    ];
    place_english_after_common_exact(&mut v, "git", (1, 2), None);
    assert_eq!(texts(&v), vec!["git", "工", "国"]);
}

/// 两项下限各管各的；仍先让过开头的完整匹配中文，只会更靠后。
#[test]
fn exact_and_complete_floors_apply_separately() {
    let thin = || {
        vec![
            zh("和", "he", true, 2),
            zh("喝", "he", true, 2),
            zh("贺", "he", true, 2),
            en("hell", "hell"),
            completion("hello", "hello"),
        ]
    };
    // 出厂：完整单词第 1、补全第 2。
    let mut v = thin();
    place_english_after_common_exact(&mut v, "hell", (1, 2), None);
    assert_eq!(texts(&v), vec!["hell", "hello", "和", "喝", "贺"]);
    // 补全压到第 4。
    let mut v = thin();
    place_english_after_common_exact(&mut v, "hell", (1, 4), None);
    assert_eq!(texts(&v), vec!["hell", "和", "喝", "hello", "贺"]);
    // 完整单词下限比补全的还靠后：补全跟在完整单词之后，不越过它。
    let mut v = thin();
    place_english_after_common_exact(&mut v, "hell", (3, 1), None);
    assert_eq!(texts(&v), vec!["和", "喝", "hell", "hello", "贺"]);

    // 开头 4 条完整匹配常用字：让过它们后已在第 5 位，下限 2 不把它往前拉。
    let common = || {
        vec![
            zh("很", "hen", true, 3),
            zh("恨", "hen", true, 3),
            zh("狠", "hen", true, 3),
            zh("痕", "hen", true, 3),
            zh("佷", "hen", false, 3),
            en("hen", "hen"),
        ]
    };
    let mut v = common();
    place_english_after_common_exact(&mut v, "hen", (2, 2), None);
    assert_eq!(texts(&v), vec!["很", "恨", "狠", "痕", "hen", "佷"]);
    // 下限超过中文条数：落在末尾。
    let mut v = common();
    place_english_after_common_exact(&mut v, "hen", (9, 9), None);
    assert_eq!(texts(&v), vec!["很", "恨", "狠", "痕", "佷", "hen"]);

    // 中文侧为空：无处可让，英文照旧居首。
    let mut v = vec![en("GitHub", "github")];
    place_english_after_common_exact(&mut v, "github", (2, 2), None);
    assert_eq!(texts(&v), vec!["GitHub"]);
}

/// 没有完整单词英文时，补全只看自己的下限：完整单词那项（5）不得把它压后。
#[test]
fn completion_alone_ignores_exact_floor() {
    let mut v = vec![
        zh("和", "he", true, 2),
        zh("喝", "he", true, 2),
        zh("贺", "he", true, 2),
        completion("hello", "hello"),
    ];
    place_english_after_common_exact(&mut v, "hell", (5, 2), None);
    assert_eq!(texts(&v), vec!["和", "hello", "喝", "贺"]);
}

/// 完整单词被下限压后时，补全从完整单词落位处起让拼音真词，不得插到真词前面。
#[test]
fn completion_yields_to_whole_words_after_floored_exact() {
    let mut v = vec![
        zh("小", "xiao", false, 4),
        zh("小米", "xiaomi", false, 5),
        zh("消灭", "xiaomie", false, 5),
        zh("晓", "xiao", false, 4),
        en("xiaom", "xiaom"),
        completion("Xiaomi", "xiaomi"),
    ];
    place_english_after_common_exact(&mut v, "xiaom", (2, 1), None);
    assert_eq!(
        texts(&v),
        vec!["小", "xiaom", "小米", "消灭", "Xiaomi", "晓"]
    );
}

/// 开头有完整匹配段时：完整单词紧随其后，补全再跟完整单词。
#[test]
fn exact_then_completion_after_lead() {
    let mut v = vec![
        zh("很", "hen", true, 3),
        zh("恨", "hen", true, 3),
        zh("和", "he", true, 2),
        en("hen", "hen"),
        completion("henry", "henry"),
    ];
    place_english_after_common_exact(&mut v, "hen", (1, 2), None);
    assert_eq!(texts(&v), vec!["很", "恨", "hen", "henry", "和"]);
}

/// 完整单词 ≥2 条时补全下限扣成 0：补全直接跟在完整单词之后，不再往后压。
#[test]
fn completion_floor_counts_exact_words() {
    let mut v = vec![
        zh("和", "he", true, 2),
        zh("喝", "he", true, 2),
        en("hell", "hell"),
        en("Hell", "hell"),
        completion("hello", "hello"),
    ];
    place_english_after_common_exact(&mut v, "hell", (1, 2), None);
    assert_eq!(texts(&v), vec!["hell", "Hell", "hello", "和", "喝"]);
}

/// 真机场景三：`github`。中文侧无候选 ⇒ 英文自然居首。
#[test]
fn english_first_when_no_chinese() {
    let mut v = vec![en("GitHub", "github")];
    place_english_after_common_exact(&mut v, "github", (1, 1), None);
    assert_eq!(texts(&v), vec!["GitHub"]);
}

/// 不得改变任何两条非英文候选的相对次序。
///
/// 这是选「后置定位」而不往 `candidate_display_order` 加比较键的理由：比较键对每一对候选
/// 生效，必然扰动中文之间的既有次序（`by_source_tier` 被 `mixed` 挡住也是同一原因）。
#[test]
fn non_english_order_untouched() {
    // 刻意乱序：常用/生僻交替，且不按 tier 排好——后置定位不该「顺手」整理它们。
    let base = vec![
        zh("很", "hen", true, 3),
        zh("𬣳", "hen", false, 3),
        zh("恨", "hen", true, 3),
        zh("哏", "hen", false, 3),
    ];
    let mut v = base.clone();
    v.push(en("hen", "hen"));
    place_english_after_common_exact(&mut v, "hen", (1, 1), None);

    let non_en: Vec<&str> = v
        .iter()
        .filter(|c| c.source != CandidateSource::English)
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(non_en, texts(&base), "非英文候选的相对次序必须原样保持");
    // 开头连续常用段只有「很」一条（第 2 条即生僻字断开），故英文插在第 2 位。
    assert_eq!(texts(&v)[1], "hen");
}

/// 引擎新合成的整句（`is_synthesized`）。
fn synth(text: &str, code: &str, consumed: usize) -> Candidate {
    Candidate {
        is_sentence: true,
        is_synthesized: true,
        ..zh(text, code, true, consumed)
    }
}

/// ②b 混合简拼整句：合成整句 + `is_abbrev`（与整串简拼词同层）。
///
/// `hello` 的「和理论哦」、`hell` 的「哈额乐乐」都属此类（he + l|l + o）。须带 `is_abbrev`，
/// 否则 `source_tier` 算出档 1，与真实流水线的档 4 脱节。
fn mixed_sentence(text: &str, code: &str, consumed: usize) -> Candidate {
    Candidate {
        is_abbrev: true,
        ..synth(text, code, consumed)
    }
}

/// 拼音方案：首选是吃满整串的高频简拼词（`bus` 的「不是」）时英文让到它后面；
/// 拼凑句不算；码表 / 混输（`false`）不受影响。
#[test]
fn pinyin_whole_word_leads_only_for_pinyin_schema() {
    let abbrev = |t: &str| Candidate {
        is_abbrev: true,
        weight: 103_977,
        ..zh(t, "bus", true, 3)
    };
    let fresh = || {
        vec![
            abbrev("不是"),
            abbrev("不少"),
            zh("不", "bu", true, 2),
            en("bus", "bus"),
        ]
    };
    let mut v = fresh();
    place_english_after_common_exact(&mut v, "bus", (1, 1), Some(&|_: &[Candidate]| false));
    assert_eq!(
        texts(&v),
        vec!["不是", "bus", "不少", "不"],
        "只让首选那一条：吃满整串的简拼词成片出现，整段让会把英文推出首页"
    );
    let mut v = fresh();
    place_english_after_common_exact(&mut v, "bus", (1, 1), None);
    assert_eq!(texts(&v)[0], "bus", "码表 / 混输口径不变");

    let mut v = vec![
        mixed_sentence("和理论哦", "hello", 5),
        zh("和", "he", true, 2),
        en("hello", "hello"),
    ];
    place_english_after_common_exact(&mut v, "hello", (1, 1), Some(&|_: &[Candidate]| false));
    assert_eq!(texts(&v)[0], "hello", "拼凑句仍不算开头段");
}

/// 首选让英文的词频门槛：系统词按权重过门槛；用户词不看权重；临时词与拼凑句不算。
#[test]
fn pinyin_head_word_needs_weight_or_user_dict() {
    let word = |w: i32| Candidate {
        is_abbrev: true,
        weight: w,
        ..zh("托尔斯泰", "test", true, 4)
    };
    assert!(
        !pinyin_head_word_outranks_english(&word(860)),
        "冷僻词库词不让"
    );
    assert!(pinyin_head_word_outranks_english(&word(
        PINYIN_HEAD_WORD_MIN_WEIGHT
    )));
    let mut user = word(0);
    user.meta.is_user_dict = true;
    assert!(pinyin_head_word_outranks_english(&user), "用户词不看权重");
    let mut temp = word(1_000_000);
    temp.meta.is_temp_dict = true;
    assert!(!pinyin_head_word_outranks_english(&temp), "临时词不算");
    let synth = Candidate {
        is_synthesized: true,
        ..word(1_000_000)
    };
    assert!(!pinyin_head_word_outranks_english(&synth));

    let mut v = vec![word(860), en("test", "test")];
    place_english_after_common_exact(&mut v, "test", (1, 1), Some(&|_: &[Candidate]| false));
    assert_eq!(texts(&v)[0], "test");
}

/// 整串是英文词时，引擎拼凑的整句让位给英文。
///
/// 如开简拼后 `hello` 出「和理论哦」、`book` 出「波哦看」、`work` 出「我人口」：
/// 都是 Viterbi 把常用单字凑起来的解读，词库里没有这个词。
#[test]
fn synthesized_sentence_yields_to_an_exact_english_word() {
    let mut v = vec![
        mixed_sentence("和理论哦", "hello", 5),
        zh("和", "he", true, 2),
        zh("喝", "he", true, 2),
        en("hello", "hello"),
    ];
    place_english_after_common_exact(&mut v, "hello", (1, 1), None);
    assert_eq!(
        texts(&v),
        vec!["hello", "和理论哦", "和", "喝"],
        "整串是英文词时，拼凑出来的整句不该占着首选"
    );
}

/// 反面：词库真实词条不让位，它有词库背书。
///
/// `data` 的「打他」是词典整词（`is_synthesized == false`）。与上一条合起来才是完整判据：
/// 降的是引擎合成的整句，不是所有整句。
#[test]
fn dictionary_word_keeps_its_place_against_english() {
    let mut v = vec![
        // 词库整词恰好被 Viterbi 选中：is_sentence 为真但 is_synthesized 为假。
        Candidate {
            is_sentence: true,
            ..zh("打他", "data", true, 4)
        },
        zh("大", "da", true, 2),
        en("data", "data"),
    ];
    place_english_after_common_exact(&mut v, "data", (1, 1), None);
    assert_eq!(
        texts(&v),
        vec!["打他", "data", "大"],
        "词库真实词条有背书，不该被英文顶掉"
    );
}

/// 英文只是前缀命中（整串还不是英文词）时，合成整句照旧不让位。
///
/// 打到 `hell` 时用户可能在打 `hello`，也可能在打拼音；让位要等整串成词那一刻。
/// 夹具整句必须消费整串（`consumed == input.len()`），否则进不了 `source_tier<=1`，
/// 测到的只是「整句本来排不到前面」。
#[test]
fn synthesized_sentence_keeps_its_place_when_english_is_only_a_prefix_hit() {
    let mut v = vec![
        mixed_sentence("哈额乐乐", "hell", 4),
        // 英文候选的 code 比输入长 ⇒ 前缀命中，不是整串精确。
        en("hello", "hello"),
    ];
    place_english_after_common_exact(&mut v, "hell", (1, 1), None);
    assert_eq!(
        texts(&v),
        vec!["哈额乐乐", "hello"],
        "英文非整串精确命中时不触发让位"
    );
}

fn completion(text: &str, code: &str) -> Candidate {
    let mut c = en(text, code);
    c.meta.from_prefix_complete = true;
    c
}

/// 拼音「英文前缀补全」补出来的英文让位给吃满整串的拼音真词：`xiaom` 的「小米」是
/// 简拼残码命中，不在精确档，但它才是用户最可能要的；只消费一部分输入的「小」不算。
#[test]
fn prefix_completion_yields_to_whole_input_words() {
    let mut v = vec![
        zh("小米", "xiaomi", false, 5),
        zh("消灭", "xiaomie", false, 5),
        zh("小", "xiao", false, 4),
        completion("Xiaomi", "xiaomi"),
    ];
    place_english_after_common_exact(&mut v, "xiaom", (1, 1), None);
    assert_eq!(texts(&v), vec!["小米", "消灭", "Xiaomi", "小"]);

    // 中文侧只有残缺解读时，英文整块上前，块内保持原序。
    let mut v = vec![
        zh("他", "ta", true, 2),
        en("table", "table"),
        completion("tables", "tables"),
    ];
    place_english_after_common_exact(&mut v, "table", (1, 1), None);
    assert_eq!(texts(&v), vec!["table", "tables", "他"]);
}

/// 让位两头都要收窄：没带补全标记的前缀英文（英文方案 / 混输自己的）不挪；被让的只认
/// 拼音真词——短语 `consumed` 为 0 会被当成吃满，不能因此把英文块拆成
/// 「精确英文 → 短语 → 前缀英文」。
#[test]
fn unmarked_english_and_phrases_do_not_split_the_block() {
    let phrase = Candidate {
        text: "大家好".into(),
        code: "da".into(),
        source: CandidateSource::Phrase,
        is_phrase: true,
        ..Default::default()
    };
    let mut v = vec![phrase.clone(), en("da", "da"), en("data", "data")];
    place_english_after_common_exact(&mut v, "da", (1, 1), None);
    assert_eq!(
        texts(&v),
        vec!["da", "data", "大家好"],
        "未带标记：整块原序"
    );

    // 带标记也只让拼音，不让短语。
    let mut v = vec![phrase, en("da", "da"), completion("data", "data")];
    place_english_after_common_exact(&mut v, "da", (1, 1), None);
    assert_eq!(texts(&v), vec!["da", "data", "大家好"]);
}

/// 没有要让的中文时英文整块原序不动——英文方案里调频把 `hello` 排到 `hell` 之前，
/// 不得被「精确恒在前」拆回去。
#[test]
fn english_only_list_keeps_its_order() {
    let mut v = vec![en("hello", "hello"), en("hell", "hell")];
    place_english_after_common_exact(&mut v, "hell", (1, 1), None);
    assert_eq!(texts(&v), vec!["hello", "hell"]);
}

/// 无英文候选 / 空输入时是空操作。
#[test]
fn noop_without_english_or_input() {
    let base = vec![zh("很", "hen", true, 3), zh("恨", "hen", true, 3)];

    let mut v = base.clone();
    place_english_after_common_exact(&mut v, "hen", (1, 1), None);
    assert_eq!(texts(&v), texts(&base), "无英文候选时不得改动列表");

    let mut v2 = base.clone();
    v2.push(en("hen", "hen"));
    let before = texts(&v2).iter().map(|s| s.to_string()).collect::<Vec<_>>();
    place_english_after_common_exact(&mut v2, "", (1, 1), None);
    assert_eq!(texts(&v2), before, "空输入时早退，不得改动列表");
}
