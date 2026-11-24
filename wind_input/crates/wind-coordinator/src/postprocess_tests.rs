use super::Pass::*;
use super::profile;

/// 全部 profile（供顺序单测遍历）。
const ALL: &[(&str, &[super::Pass])] = &[
    ("MAIN_PRE", profile::MAIN_PRE),
    ("MAIN_SHADOW", profile::MAIN_SHADOW),
    ("SENTENCE_POOL", profile::SENTENCE_POOL),
    ("COMPLETION_POOL", profile::COMPLETION_POOL),
    ("DIRECT_AUX_PREFIX", profile::DIRECT_AUX_PREFIX),
    ("DIRECT_AUX_MERGE", profile::DIRECT_AUX_MERGE),
    ("TEMP_PINYIN_PRE", profile::TEMP_PINYIN_PRE),
    ("TEMP_PINYIN_SHADOW", profile::TEMP_PINYIN_SHADOW),
    ("TEMP_ENGLISH", profile::TEMP_ENGLISH),
    ("MIX_ENGLISH_SEGMENT", profile::MIX_ENGLISH_SEGMENT),
    ("SPECIAL_PRE", profile::SPECIAL_PRE),
    ("RARE_CHAR_PRE", profile::RARE_CHAR_PRE),
    ("SPECIAL_SHADOW", profile::SPECIAL_SHADOW),
    ("REVERSE", profile::REVERSE),
];

/// 每个 profile 都必须是规范顺序（`Pass` 声明序）的严格递增子序列：既不乱序，也不重复。
#[test]
fn every_profile_follows_canonical_order() {
    for (name, p) in ALL {
        assert!(
            p.windows(2).all(|w| w[0] < w[1]),
            "{name} 违反规范顺序或重复：{p:?}"
        );
    }
}

/// ★ 单字过滤必须排在 shadow **之前**：`ShadowPin.position` 是绝对下标，记的是用户右键当时
/// 所见的列表，滤在 shadow 之后位次就对不上（出简让全踩过同一个坑）。端到端造 shadow 规则
/// 要真实 store，行为断言写不出来，故钉结构：主链的单字在 `MAIN_PRE` 里，且 `MAIN_PRE`
/// 在 `build_candidates` 里先于 `MAIN_SHADOW` 执行。
///
/// ⚠️ 后半句钉的是**文本位置**：两处锚点都在 `build_candidates` 同一个函数体内，文本序 =
/// 执行序；把调用挪进别的辅助函数时请重新核对。
#[test]
fn main_chain_single_char_runs_before_shadow() {
    assert!(profile::MAIN_PRE.contains(&SingleChar));
    assert!(!profile::MAIN_PRE.contains(&Shadow));
    const SRC: &str = include_str!("handle_candidate.rs");
    let body = &SRC[SRC
        .find("fn build_candidates(")
        .expect("找不到 build_candidates")..];
    // 锚点带尾逗号：只有 `run_passes` 的实参位置才有，注释里提到 profile 名不会被误认。
    let pre = body
        .find("crate::postprocess::profile::MAIN_PRE,")
        .expect("主链应经 MAIN_PRE");
    let shadow = body
        .find("crate::postprocess::profile::MAIN_SHADOW,")
        .expect("主链应经 MAIN_SHADOW");
    assert!(pre < shadow, "MAIN_PRE 必须先于 MAIN_SHADOW");
}

/// 补全池与整句池走**同一条**过滤链：主列表被滤空后从补全池取一条，不过单字就会
/// 「开了单字模式还是冒出一个词」；不过 shadow 就会「隐藏完当场又被补回来」。
#[test]
fn side_pools_go_through_filter_single_char_and_shadow() {
    for (name, p) in [
        ("COMPLETION_POOL", profile::COMPLETION_POOL),
        ("SENTENCE_POOL", profile::SENTENCE_POOL),
    ] {
        for pass in [Filter, SingleChar, Shadow] {
            assert!(p.contains(&pass), "{name} 缺 {pass:?}");
        }
    }
}

/// 调用点确实用的是对应 profile：只锁常量内容的话，调用点误传成别的 profile 照样绿。
#[test]
fn call_sites_use_their_profile() {
    let prod = crate::source_scan::production_code(include_str!("handle_candidate.rs"));
    for (pool, want) in [
        ("&mut completion_pool,", "COMPLETION_POOL"),
        ("&mut pool,", "SENTENCE_POOL"),
    ] {
        let at = prod
            .find(pool)
            .unwrap_or_else(|| panic!("找不到 {pool} 的 run_passes"));
        let tail = &prod[at..];
        let got = tail
            .find("profile::")
            .map(|i| &tail[i + "profile::".len()..])
            .and_then(|t| {
                t.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
            })
            .unwrap_or("");
        assert_eq!(got, want, "{pool} 那次 run_passes 用的 profile 不对");
    }
}
