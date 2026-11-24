fn dict_cand(text: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        source: CandidateSource::English,
        ..Default::default()
    }
}

fn phrase_cand(text: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        source: CandidateSource::Phrase,
        is_phrase: true,
        ..Default::default()
    }
}

/// `InDict` 只认英文词库来源：文本相同但来源是短语的那条不得让判据成立。
///
/// 出厂英文方案不加载短语（`[phrases] enabled = false`），端到端测不出，只能直接喂判据
/// 含短语的表；定制层或第三方方案打开 `[phrases]` 后才会走到。
#[test]
fn in_dict_ignores_non_dictionary_sources() {
    let raw = "zzq";
    assert!(
        !wants_raw_candidate(
            RawCandidateMode::InDict,
            raw,
            &[phrase_cand("zzq")],
            None,
            false
        ),
        "短语文本恰好等于所打原文，不代表这个词在英文词库里"
    );
    assert!(
        wants_raw_candidate(
            RawCandidateMode::InDict,
            raw,
            &[dict_cand("zzq")],
            None,
            false
        ),
        "同样的文本、来源是英文词库时，判据必须成立"
    );
    // 混表：短语在前、词库在后，仍应以词库那条为准。
    assert!(
        wants_raw_candidate(
            RawCandidateMode::InDict,
            raw,
            &[phrase_cand("zzq"), dict_cand("zzq")],
            None,
            false
        ),
        "混表下只要存在词库同名候选，判据就该成立"
    );
}

/// 另外两档不看 `dict`，恒定返回。
#[test]
fn always_and_off_ignore_the_dictionary() {
    assert!(wants_raw_candidate(
        RawCandidateMode::Always,
        "zzq",
        &[],
        None,
        false
    ));
    assert!(!wants_raw_candidate(
        RawCandidateMode::Off,
        "zzq",
        &[dict_cand("zzq")],
        None,
        false
    ));
}

/// 被屏蔽的词仍算词库词：`InDict` 下原文条不因屏蔽消失。按它本来会显示的样子比：字面，或大小写
/// 跟随开着时投影后的形态；`OK` 不因屏蔽了 `ok` 而算。
#[test]
fn in_dict_counts_blocked_word() {
    let blocked: std::collections::HashSet<String> = ["help".to_string()].into();
    let in_dict = |raw: &str, follow: bool| {
        wants_raw_candidate(RawCandidateMode::InDict, raw, &[], Some(&blocked), follow)
    };
    assert!(in_dict("help", false));
    assert!(in_dict("Help", true), "投影后 help → Help");
    assert!(!in_dict("Help", false), "不跟随大小写时 help 不显示成 Help");
    assert!(!in_dict("Hel", true), "前缀不算");
    assert!(!wants_raw_candidate(
        RawCandidateMode::InDict,
        "help",
        &[],
        None,
        false
    ));
}

use super::*;

fn texts(v: &[Candidate]) -> Vec<&str> {
    v.iter().map(|c| c.text.as_str()).collect()
}

/// 四种开关组合的产出，逐格钉住。
#[test]
fn switch_matrix() {
    assert_eq!(
        texts(&english_head_candidates("Hel", true, true)),
        vec!["Hel", "hel", "HEL"],
        "原文在最前，变形跟随（首字母大写形态等于原文，已被剔除）"
    );
    assert_eq!(
        texts(&english_head_candidates("Hel", true, false)),
        vec!["Hel"]
    );
    assert_eq!(
        texts(&english_head_candidates("Hel", false, true)),
        vec!["hel", "HEL"],
        "不要原文时只剩变形"
    );
    assert!(
        english_head_candidates("Hel", false, false).is_empty(),
        "两者皆关 = 无头部候选，调用方走既有路径"
    );
}

/// 空输入恒空——别让它产出一条空文本候选（那会在候选窗里显示成一个空行）。
#[test]
fn empty_input_yields_nothing() {
    assert!(english_head_candidates("", true, true).is_empty());
}

/// 头部候选不带 `source` / `code`：写端据此把它们排除在词频之外。
///
/// 带上 source 会往词频表里静默写一批读端永远查不中的孤儿键。
#[test]
fn head_candidates_carry_no_source_or_code() {
    for c in english_head_candidates("Hel", true, true) {
        assert_eq!(c.source, wind_candidate::CandidateSource::default());
        assert!(c.code.is_empty(), "头部候选不得带码：{}", c.text);
    }
}

// ── 大小写投影 ─────────────────────────────────────────────────────

/// 用户给的两个例子，逐格钉住。
#[test]
fn projects_user_examples() {
    assert_eq!(project_to_input_case("Hi", "hill").as_deref(), Some("Hill"));
    assert_eq!(project_to_input_case("WoW", "wow").as_deref(), Some("WoW"));
    assert_eq!(
        project_to_input_case("WoW", "wowed").as_deref(),
        Some("WoWed"),
        "超出输入长度的后缀保留词库原文"
    );
}

/// 全小写输入不投影——绝大多数击键走这条零分配快路。
#[test]
fn all_lowercase_input_projects_nothing() {
    assert_eq!(project_to_input_case("hi", "hill"), None);
    assert_eq!(project_to_input_case("", "hill"), None);
}

/// 单向：小写位不把词库自带的大写压下去，否则 `China` / `iPhone` 再也打不出来。
#[test]
fn lowercase_never_overwrites_dict_case() {
    assert_eq!(
        project_to_input_case("chi", "China"),
        None,
        "无大写输入 ⇒ 不投影"
    );
    assert_eq!(
        project_to_input_case("Iph", "iPhone").as_deref(),
        Some("IPhone"),
        "用户按了 Shift 才覆盖，且只覆盖他按了的那两位"
    );
}

/// 字母不对应 ⇒ 整条放弃，不产出杂合体。
#[test]
fn mismatched_letters_abandon_whole_candidate() {
    assert_eq!(project_to_input_case("Wo", "xyz"), None);
    assert_eq!(
        project_to_input_case("Wo", "w-o"),
        None,
        "非字母位同样按不对应处理（短语/命令类候选原样留下）"
    );
}

/// 投影结果与原文相同时返回 `None`，避免调用方白记一次 `case_source`。
#[test]
fn no_change_yields_none() {
    assert_eq!(
        project_to_input_case("Hi", "Hill"),
        None,
        "词库本就是这个形态"
    );
}

/// 三档循环闭合。
#[test]
fn variant_cycles_back_to_default() {
    let mut v = CaseVariant::Default;
    v = v.next();
    assert_eq!(v, CaseVariant::Upper);
    v = v.next();
    assert_eq!(v, CaseVariant::Lower);
    assert_eq!(v.next(), CaseVariant::Default);
}

/// 记账文本恒是投影前的词库原文——两端不同源就是英文词频静默失效。
#[test]
fn projection_keeps_original_text_for_freq() {
    let mut cands = vec![Candidate {
        text: "hill".into(),
        ..Default::default()
    }];
    apply_english_case(&mut cands, "Hi", CaseVariant::Default);
    assert_eq!(cands[0].text, "Hill", "显示与上屏用投影后");
    assert_eq!(cands[0].freq_text(), "hill", "记账用投影前");
}

/// 档位切换连续改写同一条候选时，`case_source` 仍是词库原文而非上一档的形态。
#[test]
fn variant_switch_keeps_original_not_previous_form() {
    let mut cands = vec![Candidate {
        text: "hill".into(),
        ..Default::default()
    }];
    apply_english_case(&mut cands, "Hi", CaseVariant::Default);
    apply_english_case(&mut cands, "Hi", CaseVariant::Upper);
    assert_eq!(cands[0].text, "HILL");
    assert_eq!(cands[0].freq_text(), "hill");
}

/// 全大写/全小写档整条套形，与输入形态无关。
#[test]
fn upper_and_lower_variants_ignore_input_shape() {
    let mk = || {
        vec![Candidate {
            text: "hill".into(),
            ..Default::default()
        }]
    };
    let mut up = mk();
    apply_english_case(&mut up, "hi", CaseVariant::Upper);
    assert_eq!(up[0].text, "HILL", "输入全小写也照样全大写");
    let mut low = mk();
    low[0].text = "China".into();
    apply_english_case(&mut low, "Chi", CaseVariant::Lower);
    assert_eq!(low[0].text, "china");
}

/// 去重保序保留首条。
#[test]
fn dedup_keeps_first_occurrence() {
    let mut v: Vec<Candidate> = ["Hi", "hi", "Hi", "HI"]
        .iter()
        .map(|t| Candidate {
            text: (*t).into(),
            ..Default::default()
        })
        .collect();
    dedup_by_text(&mut v);
    assert_eq!(texts(&v), vec!["Hi", "hi", "HI"]);
}

/// 头部格由同名英文词库候选占据；同名短语不算（照旧被头部吃掉），其余词库段保序。
#[test]
fn merge_head_keeps_dict_word_in_head_slot() {
    let mut word = dict_cand("hello");
    word.code = "hello".into();
    word.comment = "释义".into();
    let head = english_head_candidates("hello", true, true);
    let dict = vec![
        dict_cand("help"),
        phrase_cand("HELLO"),
        word,
        dict_cand("Hello"),
    ];
    let merged = merge_head_with_dict(head, dict);
    assert_eq!(texts(&merged), vec!["hello", "Hello", "HELLO", "help"]);
    assert_eq!(
        merged[0].source,
        CandidateSource::English,
        "原文那一格是词库词"
    );
    assert_eq!(merged[0].comment, "释义", "词库词的属性保留");
    assert_eq!(merged[1].source, CandidateSource::English, "变形格同理");
    assert_eq!(
        merged[2].source,
        CandidateSource::None,
        "同名短语不占格：头部变形照旧，短语被吃掉"
    );
}

/// 词库同时有 `asp`（词频更高）与 `ASP`，输入 `ASP`：投影后的 `asp` 与真 `ASP` 同名。
/// 占据头部格的必须是没被改写的真 `ASP`，投影出来的那条被吃掉。
#[test]
fn merge_head_prefers_unrewritten_dict_word() {
    let mut dict = vec![dict_cand("asp"), dict_cand("ASP")];
    assert!(apply_english_case(&mut dict, "ASP", CaseVariant::Default));
    assert_eq!(
        texts(&dict),
        vec!["ASP", "ASP"],
        "前提：asp 投影后与真 ASP 同名"
    );
    let merged = merge_head_with_dict(english_head_candidates("ASP", true, false), dict);
    assert_eq!(texts(&merged), vec!["ASP"]);
    assert_eq!(merged[0].source, CandidateSource::English);
    assert_eq!(
        merged[0].case_source, None,
        "头部格是真 ASP，不是投影出来的 asp"
    );
}

/// 改写后同名时保留未被改写的那条，且占首次出现的那一格（位次不变）。
#[test]
fn dedup_prefers_unrewritten_and_keeps_slot() {
    let mut v = vec![dict_cand("asp"), dict_cand("help"), dict_cand("ASP")];
    for (i, c) in v.iter_mut().enumerate() {
        c.natural_order = i as i32;
    }
    assert!(apply_english_case(&mut v, "ASP", CaseVariant::Default));
    dedup_by_text(&mut v);
    assert_eq!(texts(&v), vec!["ASP", "help"]);
    assert_eq!(v[0].case_source, None, "留下的是真 ASP");
    assert_eq!(v[0].natural_order, 0, "占首次出现那一格的位次");
    // 都改写过 / 都没改写：仍保留首条。
    let mut w = vec![dict_cand("hill"), dict_cand("HILL")];
    w[1].case_source = Some("Hill".into());
    w[0].case_source = Some("hill".into());
    w[0].text = "HILL".into();
    dedup_by_text(&mut w);
    assert_eq!(w[0].freq_text(), "hill");
}

/// 无字母的输入三形态相同，变形为空——只剩原文。
#[test]
fn non_alpha_input_has_no_variants() {
    assert_eq!(
        texts(&english_head_candidates("123", true, true)),
        vec!["123"]
    );
}
