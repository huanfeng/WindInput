use super::*;
use crate::table::AuxCodeTable;

/// 小鹤形码（取自 flypy_full.txt）：释 pl、湿 dy、适 zk、十 al、试 yg、读 yd、度 gy。
fn table() -> AuxCodeTable {
    AuxCodeTable::from_rows(vec![
        ('释', "pl"),
        ('湿', "dy"),
        ('适', "zk"),
        ('十', "al"),
        ('试', "yg"),
        ('读', "yd"),
        ('度', "gy"),
        ('国', "ky"),
        ('庆', "gd"),
        ('情', "xo"),
    ])
}

fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        ..Default::default()
    }
}

fn texts(v: &[Candidate]) -> Vec<&str> {
    v.iter().map(|c| c.text.as_str()).collect()
}

fn m(text: &str, aux: &str) -> bool {
    direct_matches(text, &table(), aux, DirectPhraseRule::Any, 0)
}

// ── 切分 ──

#[test]
fn split_odd_takes_last_one() {
    assert_eq!(
        split_direct("uiduc"),
        Some(DirectSplit {
            prefix: "uidu",
            aux: "c"
        })
    );
    assert_eq!(
        split_direct("xlr"),
        Some(DirectSplit {
            prefix: "xl",
            aux: "r"
        })
    );
}

#[test]
fn split_even_takes_last_two() {
    assert_eq!(
        split_direct("uidupl"),
        Some(DirectSplit {
            prefix: "uidu",
            aux: "pl"
        })
    );
    assert_eq!(
        split_direct("xlrn"),
        Some(DirectSplit {
            prefix: "xl",
            aux: "rn"
        })
    );
}

#[test]
fn split_needs_three_keys() {
    assert_eq!(split_direct(""), None);
    assert_eq!(split_direct("u"), None);
    assert_eq!(split_direct("ui"), None);
}

#[test]
fn split_skips_manual_separator() {
    assert_eq!(split_direct("ui'dup"), None);
    assert_eq!(split_direct("uidu'p"), None);
}

#[test]
fn split_skips_non_letter_aux() {
    // 微软双拼 `;` = ing：落在辅码位上不是辅码。前缀里的 `;` 照常放行（由引擎判音节）。
    assert_eq!(split_direct("uid;"), None);
    assert_eq!(
        split_direct("u;dup"),
        Some(DirectSplit {
            prefix: "u;du",
            aux: "p"
        })
    );
}

// ── 匹配：§5 表逐格 ──

#[test]
fn single_char_one_letter() {
    assert!(m("释", "p"));
    assert!(!m("湿", "p"));
}

#[test]
fn single_char_two_letters() {
    assert!(m("释", "pl"));
    assert!(!m("释", "pk"), "按码开头匹配：pl 不以 pk 开头");
    assert!(!m("十", "pl"));
}

#[test]
fn phrase_one_letter_any_char() {
    assert!(m("释读", "p"), "首字 释=pl");
    assert!(m("释读", "y"), "次字 读=yd，任意字规则");
    assert!(!m("湿度", "p"), "湿 dy / 度 gy 都不以 p 开头");
}

#[test]
fn phrase_two_letters_whole_code_of_any_char() {
    // ① 任一字的某码以 ab 开头
    assert!(m("释读", "yd"), "读=yd");
    assert!(m("释读", "pl"), "释=pl");
    assert!(!m("湿度", "pl"));
}

#[test]
fn phrase_two_letters_ordered_pair() {
    // ② i < j：第 i 字 a 开头、第 j 字 b 开头
    assert!(m("释读", "py"), "释 p + 读 y");
    assert!(!m("释读", "yp"), "顺序反了：读在释之后，不成立");
    assert!(m("国庆", "kg"), "国 ky + 庆 gd");
    assert!(!m("国情", "kg"), "情=xo 不以 g 开头");
    assert!(m("湿度", "dg"));
}

/// 末字规则：只看最后一个字（单字即其本身），1 / 2 位都按码开头。
#[test]
fn last_rule_checks_final_char_only() {
    let last =
        |text: &str, aux: &str| direct_matches(text, &table(), aux, DirectPhraseRule::Last, 0);
    assert!(last("释读", "y"), "读=yd");
    assert!(last("释读", "yd"));
    assert!(!last("释读", "p"), "释是首字");
    assert!(!last("释读", "py"), "不做跨字配对");
    assert!(last("国庆", "gd"));
    assert!(!last("国情", "k"));
    assert!(last("释", "pl"), "单字同 Any");
    assert!(!last("释读", ""));
}

#[test]
fn unknown_char_and_empty_lookup_do_not_match() {
    assert!(!m("李", "m"), "表里无码");
    let empty = AuxCodeTable::new();
    assert!(!direct_matches("释", &empty, "p", DirectPhraseRule::Any, 0));
    assert!(!m("释", ""), "空辅码不命中");
    assert!(!m("释", "plx"), "超过 2 位不命中");
}

/// 2 位辅码的命中集 ⊆ 其首字母 1 位的命中集。协调器据此按首字母给引擎下推准入、
/// 并让奇数键取的前缀池在紧随其后的偶数键复用——这条不成立，复用就会漏命中。
#[test]
fn two_letter_hits_are_subset_of_first_letter_hits() {
    let t = table();
    let texts = [
        "释",
        "湿",
        "适",
        "十",
        "试",
        "读",
        "度",
        "国",
        "庆",
        "情",
        "释读",
        "湿度",
        "国庆",
        "国情",
        "读释",
        "释读度",
        "李",
    ];
    let letters = ['p', 'l', 'd', 'y', 'g', 'k', 'x', 'o', 'a', 'z'];
    let mut two_letter_hits = 0;
    for text in texts {
        for a in letters {
            for b in letters {
                let ab: String = [a, b].iter().collect();
                for (max, r) in [0, 2]
                    .into_iter()
                    .flat_map(|m| [DirectPhraseRule::Any, DirectPhraseRule::Last].map(|r| (m, r)))
                {
                    if direct_matches(text, &t, &ab, r, max) {
                        two_letter_hits += 1;
                        assert!(
                            direct_matches(text, &t, &a.to_string(), r, max),
                            "{text} 中 {ab} 却不中 {a}"
                        );
                    }
                }
            }
        }
    }
    assert!(two_letter_hits > 20, "样本太少：{two_letter_hits}");
}

#[test]
fn max_phrase_len_excludes_long_phrases_only() {
    let t = table();
    let r = DirectPhraseRule::Any;
    assert!(!direct_matches("释读度", &t, "p", r, 2), "3 字词 > 上限 2");
    assert!(direct_matches("释读", &t, "p", r, 2));
    assert!(direct_matches("释", &t, "p", r, 1), "单字恒参与");
    assert!(direct_matches("释读度", &t, "p", r, 0), "0 = 不限");
}

// ── 来源资格 ──

#[test]
fn source_requires_full_prefix_cover_non_sentence() {
    let full = Candidate {
        consumed_length: 4,
        ..cand("释读")
    };
    assert!(is_direct_source(&full, 4));
    assert!(is_direct_source(&cand("释读"), 4), "0 = 整串");
    let lexicon_sentence = Candidate {
        is_sentence: true,
        ..cand("释读")
    };
    assert!(
        is_direct_source(&lexicon_sentence, 4),
        "词库词同时是整句解：照常参与"
    );
    for (why, c) in [
        (
            "没吃满前缀",
            Candidate {
                consumed_length: 2,
                ..cand("释")
            },
        ),
        (
            "子短语",
            Candidate {
                is_partial: true,
                ..cand("释")
            },
        ),
        (
            "前缀补全",
            Candidate {
                is_prefix: true,
                ..cand("释读者")
            },
        ),
        (
            "引擎合成的整句",
            Candidate {
                is_synthesized: true,
                is_sentence: true,
                ..cand("是读")
            },
        ),
        (
            "简拼",
            Candidate {
                is_abbrev: true,
                ..cand("释读")
            },
        ),
        (
            "检索范围放宽补进来的",
            Candidate {
                is_scope_filtered: true,
                ..cand("释读")
            },
        ),
        (
            "草稿",
            Candidate {
                is_draft: true,
                ..cand("释读")
            },
        ),
    ] {
        assert!(!is_direct_source(&c, 4), "{why}");
    }
}

#[test]
fn mark_direct_hit_drops_prefix_sentence_identity() {
    let mut c = Candidate {
        is_sentence: true,
        is_sentence_demoted: true,
        sentence_rank: 1,
        consumed_length: 4,
        code: "shidu".into(),
        ..cand("释读")
    };
    mark_direct_hit(&mut c, 5);
    assert_eq!(c.consumed_length, 5);
    assert!(c.is_direct_aux);
    assert!(!c.is_sentence && !c.is_sentence_demoted);
    assert_eq!(c.sentence_rank, 0, "不能冒充本次整句 N-best");
    assert_eq!(c.code, "shidu", "码仍是前缀的拼音码");
}

// ── 排序：§6 三种情况与无命中 ──

fn hit(text: &str) -> Candidate {
    Candidate {
        is_direct_aux: true,
        consumed_length: 5,
        ..cand(text)
    }
}

#[test]
fn placement_by_parity_and_syllables() {
    assert_eq!(direct_placement(1, 2), DirectPlacement::HitsFirst);
    assert_eq!(direct_placement(1, 3), DirectPlacement::AfterTop);
    assert_eq!(
        direct_placement(2, 1),
        DirectPlacement::AfterFullCover(EVEN_KEEP_FULL_COVER)
    );
    assert_eq!(EVEN_KEEP_FULL_COVER, 2);
    assert_eq!(ODD_KEEP_TOP_MIN_SYLLABLES, 3);
}

#[test]
fn no_hits_keeps_main_untouched() {
    let main = vec![cand("湿度"), cand("适度")];
    let out = merge_direct_hits(main, vec![], 5, DirectPlacement::HitsFirst);
    assert_eq!(texts(&out), vec!["湿度", "适度"]);
}

#[test]
fn odd_short_prefix_hits_first_rest_deduped() {
    let main = vec![cand("湿度"), cand("释读"), cand("适度")];
    let out = merge_direct_hits(main, vec![hit("释读")], 5, DirectPlacement::HitsFirst);
    assert_eq!(texts(&out), vec!["释读", "湿度", "适度"]);
    assert!(out[0].is_direct_aux, "去重时留命中项那条（消费整串）");
}

#[test]
fn odd_long_prefix_keeps_engine_top() {
    let main = vec![cand("整句"), cand("甲"), cand("乙")];
    let out = merge_direct_hits(main, vec![hit("命中")], 7, DirectPlacement::AfterTop);
    assert_eq!(texts(&out), vec!["整句", "命中", "甲", "乙"]);
}

#[test]
fn even_keeps_up_to_two_leading_full_cover() {
    let full = |t: &str| Candidate {
        consumed_length: 6,
        ..cand(t)
    };
    let short = |t: &str| Candidate {
        consumed_length: 4,
        ..cand(t)
    };
    let keep2 = DirectPlacement::AfterFullCover(2);
    // 三条完整候选：只留前 2 条在命中项之前。
    let main = vec![full("甲"), full("乙"), full("丙"), short("丁")];
    let out = merge_direct_hits(main, vec![hit("命中")], 6, keep2);
    assert_eq!(texts(&out), vec!["甲", "乙", "命中", "丙", "丁"]);
    // 开头不是完整候选（`pl` 不成音节）：无可保留，命中项直接排最前。
    let main = vec![short("湿度"), full("乙")];
    let out = merge_direct_hits(main, vec![hit("释读")], 6, keep2);
    assert_eq!(
        texts(&out),
        vec!["释读", "湿度", "乙"],
        "只取开头连续的完整候选"
    );
    // 子短语 / 补全 / 简拼虽吃满整串也不算完整候选。
    let abbrev = Candidate {
        is_abbrev: true,
        ..full("简拼")
    };
    let out = merge_direct_hits(vec![abbrev], vec![hit("命中")], 6, keep2);
    assert_eq!(texts(&out), vec!["命中", "简拼"]);
}

#[test]
fn kept_head_wins_over_duplicate_hit() {
    let full = Candidate {
        consumed_length: 6,
        ..cand("释读")
    };
    let out = merge_direct_hits(
        vec![full],
        vec![hit("释读"), hit("试读")],
        6,
        DirectPlacement::AfterFullCover(2),
    );
    assert_eq!(texts(&out), vec!["释读", "试读"]);
    assert!(!out[0].is_direct_aux, "保留段里已有的，命中项不再重复");
}
