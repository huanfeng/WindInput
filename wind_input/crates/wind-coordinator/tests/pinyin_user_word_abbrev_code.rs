//! 编码手填成简拼的用户词不得在简拼输入下被强制置顶。
//!
//! 引擎 step 6 按「编码 == 输入」查用户词，若把字面码 `rq` 当全拼精确命中放进完整匹配层，
//! 它会跨层压过系统简拼层的「日期」「人群」（层级是硬闸门，权重 0 也照样压过）。故输入本身
//! 是简拼（`AbbrevMatcher::is_abbreviation`）且用户词编码就是这串击键时，该用户词归入简拼层，
//! 与系统简拼候选按权重 / 词频同层竞争。全拼码用户词（`riqi`）不受影响。
//!
//! 位次在 Coordinator 层取（引擎位次 ≠ 界面位次）。词典缺失时整族静默跳过（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_pinyin() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn type_code(coord: &Coordinator, s: &str) {
    for c in s.chars() {
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn config(schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    cfg
}

fn coordinator(tag: &str, words: &[(&str, &str, i32)]) -> Arc<Coordinator> {
    coordinator_for("pinyin", tag, words)
}

/// 用户词一律写在 `pinyin` 名下：双拼与全拼共用拼音用户词库（存的是全拼码）。
fn coordinator_for(schema: &str, tag: &str, words: &[(&str, &str, i32)]) -> Arc<Coordinator> {
    let store_path = std::env::temp_dir().join(format!(
        "wind_e2e_uw_abbrev_code_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    for (code, text, weight) in words {
        store
            .add_user_word("pinyin", code, text, *weight, 0)
            .expect("写入用户词");
    }
    Coordinator::new_headless_with_store(config(schema), Some(&data_dir()), store)
}

/// 简拼码用户词（文本不在系统库）不再夺首选；系统简拼高频词「日期」排在它前面。
#[test]
fn abbrev_coded_user_word_does_not_take_top() {
    if !has_pinyin() {
        return;
    }
    let coord = coordinator("plain", &[("rq", "测试简码词", 0)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    let riqi = texts.iter().position(|t| t == "日期");
    let uw = texts.iter().position(|t| t == "测试简码词");
    assert!(
        riqi.is_some(),
        "`rq` 首页应有系统简拼词「日期」，实际: {texts:?}"
    );
    assert_ne!(uw, Some(0), "简拼码用户词不应置顶，实际: {texts:?}");
    // 用户词权重 0，可能被挤出首页（None）——那也满足「不在日期之前」。
    if let Some(u) = uw {
        assert!(
            riqi.unwrap() < u,
            "「日期」应排在用户词之前，实际: {texts:?}"
        );
    }
}

/// `$` 模板文本同样适用：展开后的文本含「年」「月」「日」，不在首位。
#[test]
fn abbrev_coded_template_user_word_does_not_take_top() {
    if !has_pinyin() {
        return;
    }
    let coord = coordinator("template", &[("rq", "$Y年$M月$D日", 0)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    let is_date = |t: &String| t.contains('年') && t.contains('月') && t.ends_with('日');
    assert!(
        !texts.first().is_some_and(is_date),
        "`$Y年$M月$D日` 简拼码用户词不应置顶，实际: {texts:?}"
    );
    let riqi = texts.iter().position(|t| t == "日期");
    assert!(riqi.is_some(), "`rq` 首页应有「日期」，实际: {texts:?}");
    if let Some(u) = texts.iter().position(is_date) {
        assert!(
            riqi.unwrap() < u,
            "「日期」应排在模板词之前，实际: {texts:?}"
        );
    }
}

/// 简拼层内仍按权重竞争：高权重的简拼码用户词可以排到系统简拼词前面
/// （降层不是废掉，它与「日期」是同层对手）。
#[test]
fn abbrev_coded_user_word_competes_by_weight_within_abbrev_layer() {
    if !has_pinyin() {
        return;
    }
    let coord = coordinator("heavy", &[("rq", "测试简码词", 2_000_000_000)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("测试简码词"),
        "高权重简拼码用户词应在简拼层内胜出，实际: {texts:?}"
    );
}

/// 反向对照：全拼码用户词行为不变——高权重时仍居首选（完整匹配层，不被降到简拼层）。
#[test]
fn full_pinyin_coded_user_word_unchanged() {
    if !has_pinyin() {
        return;
    }
    let coord = coordinator("full", &[("riqi", "测试全拼词", 2_000_000_000)]);
    type_code(&coord, "riqi");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("测试全拼词"),
        "全拼码高权重用户词应居首选，实际: {texts:?}"
    );
}

/// 击键是未打完的音节（`sh` 是 shi/shang… 的前缀）时不降层：`is_abbreviation` 对它也判真，
/// 但有近 300 条前缀补全，无条件降层会把用户词沉到候选上限之外截掉。断言取层级不变量：
/// 用户词在、且排在同串的系统简拼词（`s|h` 的「时候」、`z|h` 的「之后」）之前。
#[test]
fn syllable_prefix_coded_user_word_keeps_position() {
    if !has_pinyin() {
        return;
    }
    for (code, abbrev_word) in [("sh", "时候"), ("zh", "之后")] {
        let coord = coordinator(&format!("prefix_{code}"), &[(code, "测试声母词", 0)]);
        type_code(&coord, code);
        let all: Vec<String> = coord
            .debug_candidate_triples()
            .into_iter()
            .map(|t| t.0)
            .collect();
        let uw = all.iter().position(|t| t == "测试声母词");
        let sys = all.iter().position(|t| t == abbrev_word);
        let head = &all[..all.len().min(20)];
        assert!(
            uw.is_some(),
            "`{code}` 是音节前缀，同码用户词必须仍可达，前 20: {head:?}"
        );
        assert!(
            sys.is_some(),
            "`{code}` 下应有系统简拼词「{abbrev_word}」，前 20: {head:?}"
        );
        assert!(
            uw < sys,
            "`{code}` 同码用户词应留在完整匹配层、排在简拼词「{abbrev_word}」之前，\
             用户词位次 {uw:?}，简拼词位次 {sys:?}，前 20: {head:?}"
        );
    }
}

/// 双拼：比对基准必须是原始击键（`abbr_query`），不是转换后的全拼（`query`）。小鹤 `hd` → `hai`，
/// 用户词码 `hai` 是全拼精确命中，须留在完整匹配层居首；误比 `query` 会把它降到「还」「海」之后。
#[test]
fn shuangpin_full_pinyin_coded_user_word_not_demoted() {
    if !has_pinyin() {
        return;
    }
    let coord = coordinator_for(
        "shuangpin",
        "sp_hai",
        &[("hai", "测试双拼词", 2_000_000_000)],
    );
    type_code(&coord, "hd");
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("测试双拼词"),
        "双拼 `hd`(hai) 下码为 `hai` 的高权重用户词应居首，实际: {texts:?}"
    );
}

// ── 模板用户词的调频 ───────────────────────────────────────
//
// 模板 `$Y年$M月$D日` 经 `finalize_candidates` 展开成当天日期；词频读写两端都以展开前的源文本
// 为键（`Candidate::template_source` / `freq_text`），否则次日文本一变调频即清零。

const DATE_TEMPLATE: &str = "$Y年$M月$D日";

fn is_date(t: &str) -> bool {
    t.contains('年') && t.contains('月') && t.ends_with('日') && !t.contains('$')
}

/// 开了拼音调频的协调器（出厂 `data/config.toml` 即开；`Config::default()` 是关的），
/// 可先往词频表里灌 `(rq, text) × count`，模拟「之前某天选过」。
fn freq_coordinator(tag: &str, seed: &[(&str, u32)]) -> (Arc<Coordinator>, Arc<wind_store::Store>) {
    let store_path = std::env::temp_dir().join(format!(
        "wind_e2e_uw_abbrev_code_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    store
        .add_user_word("pinyin", "rq", DATE_TEMPLATE, 0, 0)
        .expect("写入用户词");
    for (text, count) in seed {
        for _ in 0..*count {
            store.record_freq("pinyin", "rq", text).unwrap();
        }
    }
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    (coord, store)
}

/// 选中模板词：词频记在源文本 `$Y年$M月$D日` 上，不记展开出来的当天日期。
#[test]
fn template_user_word_freq_is_keyed_by_source_text() {
    if !has_pinyin() {
        return;
    }
    let (coord, store) = freq_coordinator("tmpl_freq_key", &[]);
    type_code(&coord, "rq");
    let all = coord.debug_all_candidate_texts();
    let idx = all
        .iter()
        .position(|t| is_date(t))
        .unwrap_or_else(|| panic!("`rq` 下应能找到展开后的日期候选，实际: {all:?}"));
    let shown = all[idx].clone();
    coord.select_candidate(idx);
    let by_src = store.get_freq("pinyin", "rq", DATE_TEMPLATE).unwrap();
    assert_eq!(by_src.map(|r| r.count), Some(1), "词频应记在模板源文本上");
    assert!(
        store.get_freq("pinyin", "rq", &shown).unwrap().is_none(),
        "不应再按展开文本「{shown}」记一行（次日即成孤儿行）"
    );
}

/// 隔天仍有效：源文本上已有的词频（模拟前些天选过）今天照样把它排上来。
///
/// 对照：同样 12 次词频若记在今天的展开文本上，读端不认它，模板词留在首页之外。
#[test]
fn template_user_word_freq_survives_date_change() {
    if !has_pinyin() {
        return;
    }
    let (coord, _) = freq_coordinator("tmpl_freq_src", &[(DATE_TEMPLATE, 12)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    assert!(
        texts.first().is_some_and(|t| is_date(t)),
        "源文本上有 12 次词频，模板词应已调到首选，实际: {texts:?}"
    );

    // 今天的展开文本（取自候选本身，不在测试里另算日期格式）。
    let (probe, _) = freq_coordinator("tmpl_freq_today", &[]);
    type_code(&probe, "rq");
    let today = probe
        .debug_all_candidate_texts()
        .into_iter()
        .find(|t| is_date(t))
        .expect("`rq` 下应有展开后的日期候选");
    let (coord, _) = freq_coordinator("tmpl_freq_shown", &[(today.as_str(), 12)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    assert!(
        !texts.iter().any(|t| is_date(t)),
        "记在展开文本「{today}」上的词频不应再起作用，实际首页: {texts:?}"
    );
}

/// 右键「删除」模板词：库里存的是源文本，按展开文本删恒 miss（点了没反应）。
#[test]
fn template_user_word_can_be_deleted_from_menu() {
    if !has_pinyin() {
        return;
    }
    // 灌词频把它调到首选，好在首页上按页内序号操作。
    let (coord, store) = freq_coordinator("tmpl_delete", &[(DATE_TEMPLATE, 12)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    assert!(
        texts.first().is_some_and(|t| is_date(t)),
        "前置：模板词应在首选，实际: {texts:?}"
    );
    coord.debug_candidate_op(wind_ui_types::CandidateOp::Delete, 0);
    let left = store.get_user_words("pinyin", "rq").unwrap();
    assert!(
        !left.iter().any(|r| r.text == DATE_TEMPLATE),
        "右键删除后用户词库里不应再有模板词，剩余: {:?}",
        left.iter().map(|r| &r.text).collect::<Vec<_>>()
    );
    let after = coord.debug_all_candidate_texts();
    assert!(
        !after.iter().any(|t| is_date(t)),
        "删除后候选里不应再有日期，实际: {after:?}"
    );
}

/// 右键「置顶」模板词：规则带稳定 id，次日展开文本变了照样命中。
///
/// 模拟「前些天置顶过」：规则的 word 是过去某天的展开文本，id 是 `tmpl:{源文本}`。
#[test]
fn template_user_word_pin_survives_date_change() {
    if !has_pinyin() {
        return;
    }
    let (coord, store) = freq_coordinator("tmpl_pin_seed", &[]);
    store
        .pin_shadow(
            "pinyin",
            "rq",
            "2000年1月1日",
            Some(&format!("tmpl:{DATE_TEMPLATE}")),
            0,
        )
        .unwrap();
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    assert!(
        texts.first().is_some_and(|t| is_date(t)),
        "前些天按 id 置顶的规则今天应仍把模板词放在首选，实际: {texts:?}"
    );

    // 写端：今天右键置顶，落下的规则带的是稳定 id，而不只是当天的展开文本。
    let (coord, store) = freq_coordinator("tmpl_pin_write", &[(DATE_TEMPLATE, 4)]);
    type_code(&coord, "rq");
    let texts = coord.debug_page_texts();
    let idx = texts
        .iter()
        .position(|t| is_date(t))
        .unwrap_or_else(|| panic!("前置：模板词应在首页，实际: {texts:?}"));
    assert!(idx > 0, "前置：模板词不应已在首选，实际: {texts:?}");
    coord.debug_candidate_op(wind_ui_types::CandidateOp::MoveTop, idx);
    let rec = store
        .get_shadow_rules("pinyin", "rq")
        .unwrap()
        .expect("置顶应写下 shadow 规则");
    let want = format!("tmpl:{DATE_TEMPLATE}");
    assert!(
        rec.pinned
            .iter()
            .any(|p| p.cand_id.as_deref() == Some(want.as_str())),
        "置顶规则应带稳定 id `{want}`，实际: {:?}",
        rec.pinned
    );
}
