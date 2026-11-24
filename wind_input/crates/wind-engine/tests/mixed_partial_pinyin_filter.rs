//! 混输「拼音候选须消费整串」过滤（`schema.mix.pinyin_partial_candidates{,_overflow}`）。
//!
//! 五笔拼音混输打 `gedw`（五笔精确码「青春」）时，不滤的话候选第 2 条起全是 `ge` 的同音单字，
//! 每条只解释 4 键中的 2 键（`code=ge`、`consumed_length=2`）。
//!
//! 本文件用最小内存词典复刻整条链路，钉住四件事：
//! - 默认丢弃残码候选，但开关一开就回来（反向对照，防「过滤恒生效」的假绿）；
//! - 前缀补全不受牵连（`wanl` → 「完了」，code=`wanle`、消费整串）：判据切在「解释完整度」而非
//!   「候选类型」上，按类型禁用会把正在输入的串打死；
//! - 超码长走另一个开关，默认保留（长拼音的分步上屏要留着）；
//! - 滤掉拼音候选后 `has_pinyin` 转假，满码上屏的拼音守护随之松开——这是有意的连带，锁死取值
//!   免得被当成回归「修」回去。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;
use wind_dict::{DictManager, SystemDictLayer};
use wind_engine::codetable::{CodeTableEngine, CommitOptions};
use wind_engine::mixed::{MixConfig, MixedEngine};
use wind_engine::pinyin::Config as PinyinConfig;
use wind_engine::{Engine, PinyinEngine};

/// 五笔侧：`gedw` 是精确全码「青春」；再放一条无关词占位。
fn wubi_engine() -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    d.merge_single("gedw".into(), "青春".into(), 1683, 0);
    d.merge_single("aaaa".into(), "工".into(), 100, 0);
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
    Box::new(CodeTableEngine::new(
        4,
        CommitOptions::default(),
        Arc::new(dm),
    ))
}

/// 拼音侧：`ge` 的同音单字（残码源头）、`wanle`（前缀补全对照）、`nihao`（超码长分步对照）。
/// 简拼显式关闭——本过滤与简拼无关，两者不该互相掩盖。
fn pinyin_engine() -> PinyinEngine {
    let mut d = CodetableDict::empty();
    d.merge_single("ge".into(), "个".into(), 215733, 0);
    d.merge_single("ge".into(), "各".into(), 20609, 1);
    d.merge_single("wanle".into(), "完了".into(), 5000, 0);
    d.merge_single("nihao".into(), "你好".into(), 9000, 0);
    PinyinEngine::new(
        PinyinConfig {
            enable_abbrev: false,
            ..Default::default()
        },
        CachedDict::Memory(d),
    )
}

fn mixed(partial: bool, partial_overflow: bool) -> MixedEngine {
    MixedEngine::new(
        wubi_engine(),
        Some(Box::new(pinyin_engine())),
        None,
        MixConfig {
            pinyin_partial_candidates: partial,
            pinyin_partial_candidates_overflow: partial_overflow,
            ..Default::default()
        },
    )
}

fn texts(e: &MixedEngine, input: &str) -> Vec<String> {
    e.convert(input, 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 前置事实：拼音引擎单独看，`gedw` 确实交出 `code=ge`、只消费 2 键的候选。
/// （过滤若失效，本用例仍绿——它证明的是「有东西可滤」，不是「滤掉了」。）
#[test]
fn pinyin_alone_yields_partial_candidates_for_gedw() {
    let r = pinyin_engine().convert("gedw", 20).unwrap();
    let ge = r
        .candidates
        .iter()
        .find(|c| c.text == "个")
        .expect("拼音应给出 ge 的同音字");
    assert_eq!(ge.code, "ge");
    assert_eq!(ge.consumed_length, 2, "只解释了 4 键中的 2 键");
}

/// 默认（`pinyin_partial_candidates = false`）：`gedw` 只剩五笔精确码。
#[test]
fn partial_pinyin_dropped_by_default() {
    let t = texts(&mixed(false, true), "gedw");
    assert!(t.contains(&"青春".to_string()), "五笔精确码必须在: {t:?}");
    assert!(
        !t.iter().any(|s| s == "个" || s == "各"),
        "残码同音字不该出现: {t:?}"
    );
}

/// 反向对照：开关一开，残码候选原样回来。没有这条，上一条测的可能只是词典没数据。
#[test]
fn partial_pinyin_kept_when_enabled() {
    let t = texts(&mixed(true, true), "gedw");
    assert!(
        t.contains(&"个".to_string()),
        "开关开时残码候选应保留: {t:?}"
    );
}

/// 红线：正在输入中的拼音不得被牵连。`wanl` 的「完了」是前缀补全（code=`wanle` 比输入更长 ⇒
/// 消费整串）。这条红说明判据被写成了「按候选类型禁用」，用户打到一半就没候选。
#[test]
fn prefix_completion_survives_the_filter() {
    let t = texts(&mixed(false, true), "wanl");
    assert!(
        t.contains(&"完了".to_string()),
        "前缀补全消费整串，必须活下来: {t:?}"
    );
}

/// 超码长（`nihaom` 6 键 > 五笔 4 码）走另一个开关，默认保留：长拼音的分步上屏。
#[test]
fn overflow_keeps_partial_candidates_by_default() {
    let t = texts(&mixed(false, true), "nihaom");
    assert!(
        t.contains(&"你好".to_string()),
        "超码长默认保留部分候选（分步上屏）: {t:?}"
    );
}

/// 两档开关确实独立：把超码长那档关掉，`nihaom` 的「你好」随之消失，而码长内的那档保持 false
/// 不变，证明两处各读各的字段。
#[test]
fn overflow_filter_is_a_separate_switch() {
    let t = texts(&mixed(false, false), "nihaom");
    assert!(
        !t.contains(&"你好".to_string()),
        "超码长档关掉后部分候选应被滤: {t:?}"
    );
    // 同一配置下码长内那档依旧生效（两档同向时不互相抵消）。
    assert!(!texts(&mixed(false, false), "gedw").contains(&"个".to_string()));
}

/// 真实词库端到端验收：内存词典测得了判据，测不了残码候选的量。
///
/// 不滤时 `gedw` 的两百多条残码同音字会把混合简拼词「各单位」（`ge dan wei`）压到第 200 多位，
/// 用户视角等于不存在；过滤后它回到前几位。缺 `build_dev/data` 时静默跳过。
#[test]
fn real_dict_gedw_surfaces_the_mixed_abbrev_word() {
    use std::path::PathBuf;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../build_dev/data")
        .canonicalize()
        .ok()
        .filter(|p| p.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists());
    let Some(dir) = dir else {
        eprintln!("跳过 real_dict_gedw_surfaces_the_mixed_abbrev_word：build_dev/data 不存在");
        return;
    };

    let convert = |abbrev: bool| {
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec![
            "wubi86".to_string(),
            "pinyin".to_string(),
            "wubi86_pinyin".to_string(),
        ];
        cfg.schema.active = "wubi86_pinyin".to_string();
        cfg.schema.mix.enable_pinyin_abbrev = abbrev;
        let root = std::env::temp_dir().join(format!("wind_mix_partial_filter_{abbrev}"));
        let _ = std::fs::remove_dir_all(&root);
        let mgr = wind_engine::EngineManager::with_store_override(
            &cfg,
            Some(&dir),
            None,
            Some(root.join("overrides")),
        );
        // 生产上限（`initial_candidate_limit` 对混输取 300）。
        mgr.convert("gedw", 300)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
    };

    let off = convert(false);
    assert_eq!(off, vec!["青春".to_string()], "简拼关：只剩五笔精确码");

    let on = convert(true);
    let pos = on
        .iter()
        .position(|t| t == "各单位")
        .unwrap_or_else(|| panic!("简拼开：混合简拼词应在候选里，实际: {on:?}"));
    assert!(
        pos < 5,
        "「各单位」应进首屏（改动前是第 221 位），实际第 {} 位: {on:?}",
        pos + 1
    );
    assert_eq!(
        on.first().map(String::as_str),
        Some("青春"),
        "五笔精确码仍第一"
    );
}

/// 有意的连带：滤掉拼音候选后 `has_pinyin` 转假，`auto_commit_block_on_pinyin`
/// 那道守护便不再拦满码上屏。这里锁住「拼音候选确实不在合并结果里」这一前提事实——
/// 上屏与否还要看码表自身的意向，故不断言 `should_commit` 的具体取值，只钉守护的输入。
#[test]
fn filtered_pinyin_no_longer_feeds_the_commit_guard() {
    use wind_candidate::CandidateSource;
    let r = mixed(false, true).convert("gedw", 50).unwrap();
    assert!(
        !r.candidates
            .iter()
            .any(|c| c.source == CandidateSource::Pinyin),
        "过滤后合并结果中不应再有拼音候选"
    );
    let kept = mixed(true, true).convert("gedw", 50).unwrap();
    assert!(
        kept.candidates
            .iter()
            .any(|c| c.source == CandidateSource::Pinyin),
        "反向对照：开关开时拼音候选在场"
    );
}

/// 次方案为双拼时，「消费整串」要按击键数判。
///
/// 双拼候选的 `consumed_length` 已回映射到击键域（`nihc` 的「你好」= 4），拿转换后的全拼长度
/// （`nihao` = 5）当基准会把双拼的整串候选全部当成半截丢光。
#[test]
fn shuangpin_secondary_keeps_full_buffer_candidates() {
    use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
    let layout = Layout::from_toml(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin/xiaohe.toml"),
    )
    .expect("加载小鹤布局");
    let mut d = CodetableDict::empty();
    d.merge_single("nihao".into(), "你好".into(), 9000, 0);
    d.merge_single("ni".into(), "你".into(), 8000, 0);
    let sp = PinyinEngine::new(
        PinyinConfig {
            enable_abbrev: false,
            ..Default::default()
        },
        CachedDict::Memory(d),
    )
    .with_shuangpin(ShuangpinConverter::new(layout));
    let e = MixedEngine::new(
        wubi_engine(),
        Some(Box::new(sp)),
        None,
        MixConfig {
            pinyin_partial_candidates: false,
            ..Default::default()
        },
    );
    let t = texts(&e, "nihc");
    assert!(
        t.contains(&"你好".to_string()),
        "消费整串的双拼候选必须留下: {t:?}"
    );
    assert!(
        !t.contains(&"你".to_string()),
        "只吃 2 键的仍按设置丢弃: {t:?}"
    );
}
