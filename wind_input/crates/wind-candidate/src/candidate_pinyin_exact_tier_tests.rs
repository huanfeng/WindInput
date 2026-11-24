use super::*;
use std::cmp::Ordering;

/// 混输里「拼音精确档」的典型成员：`xu`→「需」（精确音节、常用字）。
fn pinyin_exact() -> Candidate {
    Candidate {
        text: "需".into(),
        code: "xu".into(),
        weight: 6999,
        is_common: true,
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

/// 码表前缀补全：`xu` 输入下的 `xuaj`→「弹幕」（码更长，故非精确）。
fn codetable_prefix() -> Candidate {
    Candidate {
        text: "弹幕".into(),
        code: "xuaj".into(),
        weight: 1554,
        is_common: true,
        source: CandidateSource::CodeTable,
        ..Default::default()
    }
}

/// `xu` 的字节长度（本组用例的输入）。
const XU_LEN: usize = 2;

#[test]
fn pinyin_exact_common_char_is_in_tier() {
    assert!(is_pinyin_exact_tier(&pinyin_exact(), XU_LEN));
}

/// ★★ 真机 `aaw` 现场（本判据首版栽在这里）：Viterbi 整句「啊啊」只消费 2/3 键，
/// 但 `is_partial` **是 false**（整句走 `insert(0)` 不经算 `is_partial` 的闭包，同文合并时
/// 还会主动置 false）。故「消费整串」必须直接问 `consumed_length`，不能拿 `!is_partial` 代替。
#[test]
fn partial_sentence_is_excluded_even_though_not_marked_partial() {
    let aa = Candidate {
        text: "啊啊".into(),
        code: "aa".into(),
        weight: 516,
        is_common: true,
        is_partial: false, // ← 整句路径就是这么置的，正是本用例的要害
        consumed_length: 2,
        source: CandidateSource::Pinyin,
        ..Default::default()
    };
    assert!(
        !is_pinyin_exact_tier(&aa, 3),
        "只消费 2/3 键的整句不得进精确档（`aaw` 时它会抢走首位）"
    );
    // 同一条候选，输入恰好是 `aa`（消费整串）时才够格。
    assert!(is_pinyin_exact_tier(&aa, 2), "消费整串时应进档");
}

/// `consumed_length == 0` = 引擎未标注 ⇒ 按整串算（与全仓约定一致，如 `apply_freq_rerank`
/// 的 `consumes_all`、`clear_blocked_by_candidates`）。
#[test]
fn unmarked_consumption_counts_as_whole_input() {
    let c = Candidate {
        consumed_length: 0,
        ..pinyin_exact()
    };
    assert!(is_pinyin_exact_tier(&c, 5));
}

/// ★ 生僻字不进档 —— 这是「不设条数上限、改用检索范围把关」的落点。
/// `xu` 有 329 条同音字，若生僻字一并提档，整片生僻字都会越过码表前缀补全。
#[test]
fn rare_char_is_excluded_from_tier() {
    let rare = Candidate {
        is_common: false,
        ..pinyin_exact()
    };
    assert!(
        !is_pinyin_exact_tier(&rare, XU_LEN),
        "非常用字不得进拼音精确档"
    );
}

/// 四类「非精确」召回一律不进档（各自单独翻转，避免一个漏判被别的条件掩盖）。
#[test]
fn non_exact_recalls_are_excluded_from_tier() {
    for (label, c) in [
        (
            "前缀补全",
            Candidate {
                is_prefix: true,
                ..pinyin_exact()
            },
        ),
        (
            "子短语",
            Candidate {
                is_partial: true,
                ..pinyin_exact()
            },
        ),
        (
            "简拼",
            Candidate {
                is_abbrev: true,
                ..pinyin_exact()
            },
        ),
        (
            "模糊音",
            Candidate {
                is_fuzzy: true,
                ..pinyin_exact()
            },
        ),
    ] {
        assert!(
            !is_pinyin_exact_tier(&c, XU_LEN),
            "{label}候选不得进拼音精确档"
        );
    }
}

/// 码表候选永不进本档（本档是拼音专属；码表的分层靠 `is_exact_code`）。
#[test]
fn codetable_never_enters_pinyin_tier() {
    assert!(!is_pinyin_exact_tier(&codetable_prefix(), XU_LEN));
    let ct_exact = Candidate {
        code: "xu".into(),
        is_exact_code: true,
        ..codetable_prefix()
    };
    assert!(!is_pinyin_exact_tier(&ct_exact, XU_LEN));
}

/// 核心断言：拼音精确档先于码表前缀补全，**且与权重高低无关**。
/// 现场是混输打 `xu`——码表前缀补全带 `PARTIAL_MATCH_BOOST`(500K) 而拼音 `÷100` 只剩 69，
/// 纯按权重永远输；层级键让它翻过来。
#[test]
fn pinyin_exact_outranks_codetable_prefix_regardless_of_weight() {
    let py = pinyin_exact(); // 混输里实际权重 6999/100 = 69
    let ct = Candidate {
        weight: 509_999, // 码表前缀补全的权重上限（9999 + 500K）
        ..codetable_prefix()
    };
    assert_eq!(cmp_pinyin_exact_first(&py, &ct, XU_LEN), Ordering::Less);
    assert_eq!(cmp_pinyin_exact_first(&ct, &py, XU_LEN), Ordering::Greater);
}

/// 反向锁：同档内本键不表态（交给后续权重级决），否则会掩盖档内的词频序。
#[test]
fn same_tier_is_undecided() {
    assert_eq!(
        cmp_pinyin_exact_first(&pinyin_exact(), &pinyin_exact(), XU_LEN),
        Ordering::Equal
    );
    assert_eq!(
        cmp_pinyin_exact_first(&codetable_prefix(), &codetable_prefix(), XU_LEN),
        Ordering::Equal
    );
}
