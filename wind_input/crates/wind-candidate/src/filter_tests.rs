use super::*;

/// 用户亲手标成生僻的字不吃孤儿码位保底：哪怕同码位一个常用字都没有，它也照滤。
///
/// 否则在智能档，同码位唯一的常用字被降级后保底会把它原样放回第一位。
#[test]
fn user_rare_ignores_orphan_code_fallback() {
    let mut demoted = cand("档", "sivg", CandidateSource::CodeTable, false);
    demoted.user_rare = true;
    let rare = cand("桜", "sivg", CandidateSource::CodeTable, false);

    // 前置：不带 user_rare 时，这一组无常用字 ⇒ 孤儿码位保底放行全部。
    let baseline = filter_candidates(
        vec![
            cand("档", "sivg", CandidateSource::CodeTable, false),
            rare.clone(),
        ],
        FilterMode::Smart,
    );
    assert_eq!(baseline.kept.len(), 2, "无 user_rare 时保底放行全部");

    // 带上 user_rare：那一条被滤掉，另一条仍靠保底留下（不能连累它）。
    let out = filter_candidates(vec![demoted, rare], FilterMode::Smart);
    assert_eq!(
        out.kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["桜"],
        "用户降级的字应被滤掉，同组其余候选照旧"
    );
    assert_eq!(
        out.filtered
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["档"],
        "被滤的要落进 filtered——末页翻页放宽靠它把字调回来，不是真的打不出"
    );
}

/// 组里只剩一个被降级的字时也照滤（候选可以为空）。
///
/// 空了不等于打不出：`try_relax_scope_on_page_end` 的触发条件只要求「有输入」，
/// 不要求「有候选」，故空列表按翻页键仍能放宽把它调出来。
#[test]
fn user_rare_may_empty_the_group() {
    let mut only = cand("档", "sivg", CandidateSource::CodeTable, false);
    only.user_rare = true;
    let out = filter_candidates(vec![only], FilterMode::Smart);
    assert!(out.kept.is_empty());
    assert_eq!(out.filtered.len(), 1);
}

/// 同码位还有常用字时，行为与降级前一致（这条路径本来就滤，不该因新判据变样）。
#[test]
fn user_rare_unchanged_when_group_still_has_common() {
    let mut demoted = cand("桜", "sivg", CandidateSource::CodeTable, false);
    demoted.user_rare = true;
    let common = cand("档", "sivg", CandidateSource::CodeTable, true);
    let out = filter_candidates(vec![common, demoted], FilterMode::Smart);
    assert_eq!(
        out.kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["档"]
    );
}

/// 出厂档（`Keep`）下的过滤，遮蔽同名的三参版本。
///
/// 单字素材在两档下结果相同；需要区分档位的用例（`rare_phrase_*` 一族）直接调
/// [`super::filter_candidates`]。
fn filter_candidates(candidates: Vec<Candidate>, mode: FilterMode) -> FilterOutcome {
    super::filter_candidates(candidates, mode, RarePhrasePolicy::Keep)
}

fn cand(text: &str, code: &str, source: CandidateSource, is_common: bool) -> Candidate {
    Candidate {
        text: text.into(),
        code: code.into(),
        source,
        is_common,
        ..Default::default()
    }
}

/// 只取保留集——多数用例关心的是「显示成什么样」。被滤集另有专门用例覆盖。
fn kept_of(candidates: Vec<Candidate>, mode: FilterMode) -> Vec<Candidate> {
    filter_candidates(candidates, mode).kept
}

#[test]
fn smart_keeps_uncommon_codetable_when_common_pinyin_shares_code() {
    // 混输：码表生僻字 佢 与常用拼音 往/王 共用 code "wang"（不同来源）。
    // 智能过滤按来源隔离 → 佢 不被拼音的常用性挤掉（与纯五笔一致）。
    let out = kept_of(
        vec![
            cand("佢", "wang", CandidateSource::CodeTable, false),
            cand("往", "wang", CandidateSource::Pinyin, true),
            cand("王", "wang", CandidateSource::Pinyin, true),
        ],
        FilterMode::Smart,
    );
    assert!(
        out.iter().any(|c| c.text == "佢"),
        "混输下生僻码表字应保留（与纯五笔一致）"
    );
    assert!(out.iter().any(|c| c.text == "往"));
}

#[test]
fn smart_filters_uncommon_within_same_source_and_code() {
    // 同来源同 code：有常用则滤非常用（原语义不变）。
    let out = kept_of(
        vec![
            cand("王", "wang", CandidateSource::Pinyin, true),
            cand("尪", "wang", CandidateSource::Pinyin, false),
        ],
        FilterMode::Smart,
    );
    assert!(out.iter().any(|c| c.text == "王"));
    assert!(
        !out.iter().any(|c| c.text == "尪"),
        "同来源同码有常用则滤非常用"
    );
}

#[test]
fn filter_mode_config_round_trip() {
    // 菜单切换靠 as_config 写回配置、启动/热重载靠 from_config 读回；两者必须互逆，
    // 否则会出现「菜单选了全部字符、重启回到智能」这类静默回退。
    for m in [FilterMode::Smart, FilterMode::General, FilterMode::Gb18030] {
        assert_eq!(FilterMode::from_config(m.as_config()), m);
    }
    // 配置值集须与 wind-setting 的 select options 一致（smart/general/gb18030）。
    assert_eq!(FilterMode::Smart.as_config(), "smart");
    assert_eq!(FilterMode::General.as_config(), "general");
    assert_eq!(FilterMode::Gb18030.as_config(), "gb18030");
}

/// 同一个字不得「打前缀出得来、打全码反而没了」。五笔 `桜`(sivg) 与常用字「档」同码，
/// 「档」另有简码 `siv`：去重必须把被弃条目的码位并进幸存者（`merged_codes`），
/// 否则打 `siv` 时 sivg 组只剩「桜」成孤儿码被放行。
#[test]
fn smart_filters_uncommon_when_common_shares_code_via_merged() {
    // 打 siv：「档」主码 siv，被去重吃掉的 sivg 记在 merged_codes
    let mut dang = cand("档", "siv", CandidateSource::CodeTable, true);
    dang.merged_codes = vec!["sivg".into()];
    let out = kept_of(
        vec![dang, cand("桜", "sivg", CandidateSource::CodeTable, false)],
        FilterMode::Smart,
    );
    assert!(
        !out.iter().any(|c| c.text == "桜"),
        "sivg 码位有常用字「档」（经 merged_codes 还原），生僻的「桜」应与打全码时一样被滤"
    );
    assert!(out.iter().any(|c| c.text == "档"));
}

/// 反向对照：`merged_codes` 只还原真实存在的同码位关系，「有常用字就滤掉所有生僻字」
/// 的实现会让 `sivs` 的「樑」等孤儿码字被误滤。
#[test]
fn smart_keeps_uncommon_when_common_does_not_share_its_code() {
    // 「档」只占 siv/sivg，与「樑」的 sivs 无关 → 樑 仍是孤儿码，须保留
    let mut dang = cand("档", "siv", CandidateSource::CodeTable, true);
    dang.merged_codes = vec!["sivg".into()];
    let out = kept_of(
        vec![dang, cand("樑", "sivs", CandidateSource::CodeTable, false)],
        FilterMode::Smart,
    );
    assert!(
        out.iter().any(|c| c.text == "樑"),
        "sivs 下无常用字，孤儿码生僻字不该被别的码位牵连滤掉"
    );
}

/// 归并的传递性：去重是链式的（跨层 → 引擎层 → 协调器），同一个字可被连续吃三次。
/// 若 `absorb` 只取对方的 `code`、不取对方已归并的码位，中间那次的码位会静默丢失。
#[test]
fn absorb_codes_is_transitive() {
    let mut a = cand("档", "siv", CandidateSource::CodeTable, true);
    let mut b = cand("档", "sivg", CandidateSource::CodeTable, true);
    b.absorb_code("sivx"); // b 之前吃掉过 sivx
    a.absorb_codes_from(&b);
    assert!(a.merged_codes.contains(&"sivg".to_string()), "对方主码");
    assert!(
        a.merged_codes.contains(&"sivx".to_string()),
        "对方已归并的码位不能丢"
    );
    // 自身主码不重复记，重复归并幂等
    a.absorb_codes_from(&b);
    assert!(!a.merged_codes.contains(&"siv".to_string()));
    assert_eq!(a.merged_codes.len(), 2, "重复归并应幂等");
}

/// 被滤集是「检索范围放宽」的唯一数据来源（自动补充从中回补、手动放宽整体并回），
/// 必须完整且保持原序——顺序即是放宽后的呈现顺序，乱序会让补充项的位置不可预期。
#[test]
fn filtered_set_captures_removed_candidates_in_order() {
    let mut dang = cand("档", "siv", CandidateSource::CodeTable, true);
    dang.merged_codes = vec!["sivg".into()];
    let out = filter_candidates(
        vec![
            dang,
            cand("桜", "sivg", CandidateSource::CodeTable, false),
            cand("樑", "sivs", CandidateSource::CodeTable, false),
            cand("醇", "sivw", CandidateSource::CodeTable, false),
        ],
        FilterMode::Smart,
    );
    let texts = |v: &[Candidate]| v.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
    // 樑/醇 是孤儿码得以保留；桜 因同码位有常用「档」（经 merged_codes 还原）被滤
    assert_eq!(texts(&out.kept), vec!["档", "樑", "醇"]);
    assert_eq!(texts(&out.filtered), vec!["桜"]);
}

/// 保留集与被滤集恰好构成原集合的划分：不重不漏。
/// 若哪天改成「过滤时顺手丢弃某类候选」，放宽就会补不回它，且没有任何现象提示。
#[test]
fn kept_and_filtered_partition_the_input() {
    let input = vec![
        cand("王", "wang", CandidateSource::Pinyin, true),
        cand("尪", "wang", CandidateSource::Pinyin, false),
        cand("往", "wang", CandidateSource::Pinyin, true),
    ];
    let n = input.len();
    for mode in [FilterMode::Smart, FilterMode::General, FilterMode::Gb18030] {
        let out = filter_candidates(input.clone(), mode);
        assert_eq!(
            out.kept.len() + out.filtered.len(),
            n,
            "{mode:?}: 保留集+被滤集须等于原集合，不重不漏"
        );
    }
}

/// `Gb18030`（不过滤）下被滤集恒空——该档位没有「放宽」可言，补充逻辑应自然退化为无操作。
#[test]
fn gb18030_yields_empty_filtered_set() {
    let out = filter_candidates(
        vec![cand("桜", "sivg", CandidateSource::CodeTable, false)],
        FilterMode::Gb18030,
    );
    assert_eq!(out.kept.len(), 1);
    assert!(out.filtered.is_empty(), "不过滤档不该产生被滤集");
}

/// 跨来源不得归并码位（`smart_keeps_uncommon_codetable_when_common_pinyin_shares_code` 的另一端）：
/// 混输下把拼音码并进码表候选，`(CodeTable, "wang")` 会被凭空标为有常用字而误滤码表生僻字。
#[test]
fn absorb_ignores_cross_source_codes() {
    let mut ct = cand("档", "sivg", CandidateSource::CodeTable, true);
    let py = cand("档", "dang", CandidateSource::Pinyin, true);
    ct.absorb_codes_from(&py);
    assert!(
        ct.merged_codes.is_empty(),
        "拼音码不得并入码表候选，实际: {:?}",
        ct.merged_codes
    );
    // 同来源仍照常归并（守卫不能把主逻辑一起挡掉）
    let same = cand("档", "siv", CandidateSource::CodeTable, true);
    ct.absorb_codes_from(&same);
    assert_eq!(ct.merged_codes, vec!["siv".to_string()]);
}

#[test]
fn smart_keeps_orphan_uncommon_code() {
    // 孤儿编码（无常用同码）：保留全部（纯五笔生僻字场景）。
    let out = kept_of(
        vec![cand("佢", "wtn", CandidateSource::CodeTable, false)],
        FilterMode::Smart,
    );
    assert!(out.iter().any(|c| c.text == "佢"));
}

// ── 含生僻字的词（input.rare_phrase）──────────────────────────────────
mod rare_phrase {
    use super::*;

    /// 含一个表外字的词（「磺」不在通用规范汉字表内，`mark_common` 给整条置 `is_common = false`；
    /// 端到端见 `wind-coordinator/tests/rare_phrase_scope.rs`）。
    fn rare_word(code: &str, source: CandidateSource) -> Candidate {
        cand("苯磺酸", code, source, false)
    }

    /// 常用字档：出厂 `Keep` 放行整词，`Filter` 档才是旧行为。
    #[test]
    fn general_keeps_the_word_but_filter_policy_drops_it() {
        let input = vec![rare_word("taotie", CandidateSource::Pinyin)];
        let keep = super::super::filter_candidates(
            input.clone(),
            FilterMode::General,
            RarePhrasePolicy::Keep,
        );
        assert_eq!(
            keep.kept
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["苯磺酸"],
            "出厂档：词库收录的词整条打得出来（论坛 t103）"
        );
        let strict =
            super::super::filter_candidates(input, FilterMode::General, RarePhrasePolicy::Filter);
        assert!(
            strict.kept.is_empty() && strict.filtered.len() == 1,
            "Filter 档须原样保留 0.121 及以前的行为，否则这个配置项没有意义"
        );
    }

    /// 智能档的真实现场：五笔 4 码位上，词与常用单字同码 ⇒ 旧行为把词滤掉。
    #[test]
    fn smart_keeps_the_word_even_when_common_char_shares_the_code() {
        let input = vec![
            cand("档", "sivg", CandidateSource::CodeTable, true),
            rare_word("sivg", CandidateSource::CodeTable),
        ];
        let keep = super::super::filter_candidates(
            input.clone(),
            FilterMode::Smart,
            RarePhrasePolicy::Keep,
        );
        assert!(
            keep.kept.iter().any(|c| c.text == "苯磺酸"),
            "同码有常用字时词也要留下——这正是默认档下 A2-6 的触发路径"
        );
        let strict =
            super::super::filter_candidates(input, FilterMode::Smart, RarePhrasePolicy::Filter);
        assert!(
            !strict.kept.iter().any(|c| c.text == "苯磺酸"),
            "Filter 档下仍按同码遮蔽滤掉（反向对照，防止豁免写成无条件放行）"
        );
    }

    /// 放行的词不得把同码位的生僻单字顶掉：豁免若写进 `build_has_common`，这一组会从
    /// 「孤儿码位保底放行」变成「只留常用」，原来打得出的字反而没了。
    #[test]
    fn admitted_word_does_not_shadow_rare_single_chars() {
        let out = super::super::filter_candidates(
            vec![
                rare_word("sivg", CandidateSource::CodeTable),
                cand("桜", "sivg", CandidateSource::CodeTable, false),
            ],
            FilterMode::Smart,
            RarePhrasePolicy::Keep,
        );
        assert_eq!(
            out.kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["苯磺酸", "桜"],
            "词靠豁免留下、生僻单字仍靠孤儿码位保底留下，两条互不影响"
        );
    }

    /// 词里含用户标成生僻的字时一并放行；同一个降级动作对单字仍然照滤，词豁免不松掉
    /// `user_rare` 判据。
    #[test]
    fn user_rare_chars_do_not_block_the_word() {
        let mut word = rare_word("sivg", CandidateSource::CodeTable);
        word.user_rare = true;
        let mut demoted_char = cand("桜", "sivg", CandidateSource::CodeTable, false);
        demoted_char.user_rare = true;
        let out = super::super::filter_candidates(
            vec![word, demoted_char],
            FilterMode::Smart,
            RarePhrasePolicy::Keep,
        );
        assert_eq!(
            out.kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["苯磺酸"],
            "词照放行；用户亲手降级的单字仍照滤（设了要有反应）"
        );
    }

    /// 「多字」按字素簇算：单个 emoji 不是词，不吃豁免。
    ///
    /// `⚽️`、`👨‍👩‍👧` 码位数大于 1，按 `chars().count() > 1` 判会把它们当词放行、绕开检索范围。
    #[test]
    fn counts_grapheme_clusters_not_code_points() {
        for t in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳", "1️⃣"] {
            let c = cand(t, "x", CandidateSource::CodeTable, false);
            assert!(
                !rare_phrase_admits(&c, RarePhrasePolicy::Keep),
                "{t:?} 在屏幕上是一个字，不该按词豁免"
            );
        }
        let word = cand("😀😀", "x", CandidateSource::CodeTable, false);
        assert!(
            rare_phrase_admits(&word, RarePhrasePolicy::Keep),
            "两个字素簇才算词（反向对照，防止把上一条写成「凡 emoji 皆不豁免」）"
        );
    }

    /// 划分不重不漏在两档下都成立：放宽靠被滤集回补，漏一条就补不回来。
    #[test]
    fn kept_and_filtered_still_partition_the_input() {
        let input = vec![
            cand("档", "sivg", CandidateSource::CodeTable, true),
            rare_word("sivg", CandidateSource::CodeTable),
            cand("桜", "sivg", CandidateSource::CodeTable, false),
        ];
        for mode in [FilterMode::Smart, FilterMode::General, FilterMode::Gb18030] {
            for policy in [RarePhrasePolicy::Keep, RarePhrasePolicy::Filter] {
                let out = super::super::filter_candidates(input.clone(), mode, policy);
                assert_eq!(
                    out.kept.len() + out.filtered.len(),
                    input.len(),
                    "{mode:?}/{policy:?}: 保留集+被滤集须等于原集合"
                );
            }
        }
    }

    /// 配置往返互逆 + 未知值回落 `Keep`（与 `Default` 同源）。
    #[test]
    fn config_round_trip() {
        for p in [RarePhrasePolicy::Keep, RarePhrasePolicy::Filter] {
            assert_eq!(RarePhrasePolicy::from_config(p.as_config()), p);
        }
        // 值集须与 wind-setting 的 select options 一致（keep/filter）。
        assert_eq!(RarePhrasePolicy::Keep.as_config(), "keep");
        assert_eq!(RarePhrasePolicy::Filter.as_config(), "filter");
        assert_eq!(
            RarePhrasePolicy::from_config("拼错了"),
            RarePhrasePolicy::default(),
            "配错的值与没配的值必须落到同一档"
        );
    }
}

// ── 单字输入（single_char）─────────────────────────────────────────────
mod single_char {
    use super::*;

    fn admits(text: &str) -> bool {
        single_char_admits(&cand(text, "x", CandidateSource::CodeTable, false))
    }

    #[test]
    fn keeps_single_and_drops_words() {
        assert!(admits("大"));
        assert!(!admits("大家"));
        assert!(!admits("大家好"));
    }

    /// 「一个字」按字素簇算，不按码位数。
    ///
    /// `⚽️`、`👨‍👩‍👧` 码位数大于 1 却是一个图形，不能按 `chars().count() == 1` 判。
    #[test]
    fn counts_grapheme_clusters_not_code_points() {
        for t in ["⚽️", "👍🏻", "👨‍👩‍👧", "🇨🇳", "1️⃣"] {
            assert!(admits(t), "{t:?} 在屏幕上是一个字，单字模式应放行");
        }
    }

    /// 短语 / 命令 / 分组豁免：用户自己配的快捷方式不受单字模式管辖。
    #[test]
    fn user_authored_candidates_are_exempt() {
        let mut phrase = cand("今天天气不错", "fj", CandidateSource::Phrase, false);
        phrase.is_phrase = true;
        let mut command = cand("重启服务", "cc", CandidateSource::None, false);
        command.is_command = true;
        let mut group = cand("符号", "fh", CandidateSource::Phrase, false);
        group.is_group = true;

        for c in [&phrase, &command, &group] {
            assert!(
                single_char_admits(c),
                "单字模式不该滤掉用户自己配的 {:?}",
                c.text
            );
        }
    }

    /// 常用词不豁免：豁免判据写成 `is_common_like` 的话，逐字皆常用的词（`is_common = true`）
    /// 会一并放行，单字模式整体失效。
    #[test]
    fn common_words_are_not_exempt() {
        let common_word = cand("中国", "x", CandidateSource::CodeTable, true);
        assert!(
            !single_char_admits(&common_word),
            "常用词也是词，单字模式必须滤掉——豁免判据不能带上 is_common"
        );

        let common_char = cand("大", "x", CandidateSource::CodeTable, true);
        assert!(
            single_char_admits(&common_char),
            "常用字照留（反向对照，防止把上一条写成「凡 is_common 皆滤」）"
        );
    }
}

fn wc(text: &str, code: &str, is_common: bool) -> Candidate {
    Candidate {
        is_wildcard: true,
        ..cand(text, code, CandidateSource::CodeTable, is_common)
    }
}

fn wc_texts(v: &[Candidate]) -> Vec<&str> {
    v.iter().map(|c| c.text.as_str()).collect()
}

/// spec codetable-wildcard §11：通配结果整份一组——有常用字就滤生僻字，不因散在不同码位
/// 而各自当孤儿放行。现场：混输 `hanz` 首页全是 `han?` 下只有生僻字的码位。
#[test]
fn smart_treats_all_wildcard_results_as_one_group() {
    let out = filter_candidates(
        vec![
            wc("虑", "hand", true),
            wc("眓", "hanf", false),
            wc("虙", "hanm", false),
        ],
        FilterMode::Smart,
    );
    assert_eq!(wc_texts(&out.kept), ["虑"]);
    assert_eq!(
        wc_texts(&out.filtered),
        ["眓", "虙"],
        "被滤的仍进 filtered——末页放宽靠它追加回来"
    );
}

/// 通配组里一个常用字都没有 ⇒ 整组保底放行（与按码分组的孤儿码位同一条保底）。
/// 代价是扩充后前页可能移位：后一批带进常用字，这些生僻字就被滤走。
#[test]
fn smart_wildcard_group_without_common_keeps_all_until_common_arrives() {
    let rare = vec![wc("眓", "hanf", false), wc("虙", "hanm", false)];
    assert_eq!(
        kept_of(rare.clone(), FilterMode::Smart).len(),
        2,
        "无常用字：整组保底"
    );
    let mut more = rare;
    more.push(wc("虑", "hand", true));
    assert_eq!(wc_texts(&kept_of(more, FilterMode::Smart)), ["虑"]);
}

/// 通配组与按码分组互不串。通配常用字的 `merged_codes` 不得往
/// `(source, code)` 组里写，否则同码位的非通配生僻字（孤儿码位）会被它遮蔽；
/// 拼音孤儿码位同样不受通配常用字影响。
#[test]
fn wildcard_group_does_not_leak_into_code_groups() {
    let mut common = wc("虑", "hand", true);
    common.merged_codes = vec!["hanf".into()];
    let out = kept_of(
        vec![
            common,
            cand("佢", "hanf", CandidateSource::CodeTable, false),
            cand("尪", "hanz", CandidateSource::Pinyin, false),
        ],
        FilterMode::Smart,
    );
    assert_eq!(wc_texts(&out), ["虑", "佢", "尪"]);
}
