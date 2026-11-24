//! Mix（快捷输入 / 融合方案）的用户数据归属：词频与候选调整按成员方案落桶。
//!
//! Mix 是融合方案，一次列表里混着算式、日期、拼音、英文、生僻字，成员可以是第三方方案。两条纪律：
//!
//! - 用户数据按成员方案归属，不按 active：按 active 会把拼音成员学到的东西记进主方案（常是五笔）
//!   的桶。读端在 `update_mix_candidates` 里逐成员段应用，写端按候选来源反查（`mix_candidate_owner`），
//!   两端落同一个桶。
//! - 应用粒度是「成员段内」而非整张列表：拼音里调到第 3 位说的是「拼音候选里的第 3」，mix 的第 3 位
//!   可能是算式；段内应用不动成员之间的次序（成员顺序即候选优先级）。隐藏（`deleted`）不受此限。
//!
//! 检索范围过滤（`mark_common`/`apply_filter`）刻意不接：mix 成员可以是第三方方案，还有自带准入的
//! 生僻字成员，主路径的统一常用度判据与之冲突。故本文件不测过滤，那不是缺口。
//!
//! 词库缺失时整族静默跳过，判据是耗时（真跑约 1s 量级、跳过 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/pinyin.schema.toml").exists()
        && d.join("schemas/pinyin/cn_dicts/41448.dict.yaml").exists()
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

/// 五笔主方案（active ≠ 拼音成员，否则「按 active 归属」的错误实现也能通过）。
///
/// 拼音调频必须显式打开：`Config::default()` 不读 `data/config.toml`，那里默认是 false，
/// 不设的话测的是一个关着的功能。
fn mix_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.frequency.enabled = true;
    cfg
}

fn fresh_store(name: &str) -> (Arc<wind_store::Store>, PathBuf) {
    let path = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&path);
    (Arc::new(wind_store::Store::open(&path).unwrap()), path)
}

/// `;` 进快捷输入、打码，返回候选。
fn mix_candidates(store: Arc<wind_store::Store>, input: &str) -> Vec<String> {
    mix_candidates_with(mix_config(), store, input)
}

/// 同上，但用调用方给的配置（需要另开某个成员方案的开关时用）。
fn mix_candidates_with(cfg: Config, store: Arc<wind_store::Store>, input: &str) -> Vec<String> {
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    coord.handle_key_event_policed(&key_event(0xBA)); // ';'
    for c in input.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    coord.debug_all_candidate_texts()
}

/// 同上但选走首候选（触发记账），返回被选中的文本。
fn mix_commit_first(store: Arc<wind_store::Store>, input: &str) -> String {
    let coord = Coordinator::new_headless_with_store(mix_config(), Some(&data_dir()), store);
    coord.handle_key_event_policed(&key_event(0xBA));
    for c in input.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    let first = coord
        .debug_all_candidate_texts()
        .first()
        .cloned()
        .unwrap_or_default();
    coord.handle_key_event_policed(&key_event(0x20)); // 空格
    first
}

fn baseline(tag: &str, input: &str) -> Option<Vec<String>> {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return None;
    }
    let (store, path) = fresh_store(&format!("wind_mix_base_{tag}.redb"));
    let v = mix_candidates(store, input);
    let _ = std::fs::remove_file(&path);
    Some(v)
}

/// 隐藏：拼音方案里隐藏掉的候选，快捷输入里也不该冒出来。
#[test]
fn hidden_in_pinyin_bucket_is_hidden_in_mix() {
    let Some(base) = baseline("hide", "ni") else {
        return;
    };
    assert!(!base.is_empty(), "前提：mix `ni` 应有候选");
    let victim = base[0].clone();

    let (store, path) = fresh_store("wind_mix_hide.redb");
    store
        .delete_shadow("pinyin", "ni", &victim)
        .expect("delete_shadow 失败");
    let after = mix_candidates(store, "ni");
    assert!(
        !after.contains(&victim),
        "拼音桶里隐藏掉的 {victim:?} 不该出现在快捷输入里（实际前 6: {:?}）",
        &after[..6.min(after.len())]
    );
    assert!(!after.is_empty(), "隐藏一条不应清空列表");
    let _ = std::fs::remove_file(&path);
}

/// 置顶：作用在成员段内。拼音成员段的首条应变成被置顶的那个。
#[test]
fn pin_in_pinyin_bucket_reorders_within_member_segment() {
    let Some(base) = baseline("pin", "ni") else {
        return;
    };
    assert!(base.len() >= 3, "前提：mix `ni` 应有足够候选");
    let target = base[2].clone();
    assert_ne!(base[0], target, "前提：目标须原本不在首位");

    let (store, path) = fresh_store("wind_mix_pin.redb");
    store
        .pin_shadow("pinyin", "ni", &target, None, 0)
        .expect("pin_shadow 失败");
    let after = mix_candidates(store, "ni");
    assert_eq!(
        after.first().map(|s| s.as_str()),
        Some(target.as_str()),
        "拼音成员段内的置顶应生效（实际前 6: {:?}）",
        &after[..6.min(after.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// 反向对照：带 store 但不写规则 ⇒ 顺序回到原序。缺了它，「凡是接上 store 就变序」的实现会让
/// 上面两条假绿。
#[test]
fn store_without_rule_keeps_mix_order() {
    let Some(base) = baseline("nopin", "ni") else {
        return;
    };
    let (store, path) = fresh_store("wind_mix_nopin.redb");
    let after = mix_candidates(store, "ni");
    let n = 10.min(base.len()).min(after.len());
    assert_eq!(&after[..n], &base[..n], "无规则时顺序不得变化");
    let _ = std::fs::remove_file(&path);
}

/// 变异防线：规则写进 active 方案（`wubi86`）桶 ⇒ mix 不得生效。归属若退回 active，本用例与上面
/// 两条会从相反方向同时变红。
#[test]
fn rule_in_active_schema_bucket_does_not_leak_into_mix() {
    let Some(base) = baseline("leak", "ni") else {
        return;
    };
    assert!(base.len() >= 3, "前提：mix `ni` 应有足够候选");
    let target = base[2].clone();

    let (store, path) = fresh_store("wind_mix_leak.redb");
    store
        .pin_shadow("wubi86", "ni", &target, None, 0)
        .expect("pin_shadow 失败");
    let after = mix_candidates(store, "ni");
    assert_eq!(
        after.first().map(|s| s.as_str()),
        base.first().map(|s| s.as_str()),
        "写在五笔桶里的规则不该影响 mix 的拼音成员（实际前 6: {:?}）",
        &after[..6.min(after.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// 写端：mix 里选走拼音候选，词频记进 `"pinyin"` 桶，不落主方案桶；读写不同桶时记账看着成功而
/// 顺序永不动。
#[test]
fn mix_freq_lands_in_member_bucket_not_active_schema() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let (store, path) = fresh_store("wind_mix_freq.redb");
    let picked = mix_commit_first(Arc::clone(&store), "ni");
    assert!(!picked.is_empty(), "前提：mix `ni` 应有候选可选");

    assert!(
        store.get_freq("pinyin", "ni", &picked).unwrap().is_some(),
        "mix 里选走的拼音候选应记进 pinyin 桶（选中 {picked:?}）"
    );
    assert!(
        store.get_freq("wubi86", "ni", &picked).unwrap().is_none(),
        "不得记进主方案桶——成员各自的用户数据归各自（选中 {picked:?}）"
    );
    let _ = std::fs::remove_file(&path);
}

/// 闭环：在拼音方案里学到的词频，快捷输入里照样生效（同一个桶的读端）。
#[test]
fn freq_learned_in_pinyin_schema_reranks_mix() {
    let Some(base) = baseline("freq", "ni") else {
        return;
    };
    assert!(base.len() >= 3, "前提：mix `ni` 应有足够候选");
    let target = base[2].clone();
    assert_ne!(base[0], target, "前提：目标须原本不在首位");

    let (store, path) = fresh_store("wind_mix_freq_read.redb");
    for _ in 0..5 {
        store
            .record_freq("pinyin", "ni", &target)
            .expect("record_freq 失败");
    }
    let after = mix_candidates(store, "ni");
    assert_eq!(
        after.first().map(|s| s.as_str()),
        Some(target.as_str()),
        "pinyin 桶里的词频应在 mix 的拼音成员段内生效（实际前 6: {:?}）",
        &after[..6.min(after.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// 取数窗口 ≠ 每成员配额。
///
/// 词频重排与候选调整只在已取到的那批候选里工作（`apply_freq_rerank_in` 不回查词库），「问引擎要
/// 多少条」就是它们的可见窗口。窗口若等于配额，排在配额之外的词永远等不到被提上来（临英里一码就
/// 置顶的词，快捷输入要多打一码才生效）。
///
/// 判据是重排前的原序位次而非重排后名次，且必须落在配额之外的词上：拿本来就在前 50 的词断言，
/// 「先截后排」的实现同样能通过，故下面先把「原序在配额之外」断成前提。
#[test]
fn freq_reaches_member_candidates_beyond_quota() {
    if !has_schemas() || !data_dir().join("schemas/english").is_dir() {
        eprintln!("跳过：词库不存在");
        return;
    }
    // 英文成员的词频要显式打开：`Config::default()` 里是 false（同本文件开头那条假绿告警）。
    let mut cfg = mix_config();
    cfg.schema.english.frequency.enabled = true;

    // 观测词取没有词频名次的 `hitherto`：英文候选按名次排序后，常用的 `history` 已进配额。
    // 前提①：`hitherto` 原本不在候选面里（即原序排在本成员配额之外）。
    let (store, path) = fresh_store("wind_mix_quota_base.redb");
    let base = mix_candidates_with(cfg.clone(), Arc::clone(&store), "hi");
    assert!(base.len() >= 2, "前提：mix `hi` 应有候选");
    assert!(
        !base.contains(&"hitherto".to_string()),
        "前提失效：`hitherto` 已在配额内，这条用例就测不出窗口问题了（前 8: {:?}）",
        &base[..8.min(base.len())]
    );
    let _ = std::fs::remove_file(&path);

    // 前提②：英文词频记在 `"english"` 桶、码为候选自身的词库码（见 `freq_code` 的来源分流）。
    let (store, path) = fresh_store("wind_mix_quota_freq.redb");
    for _ in 0..5 {
        store
            .record_freq("english", "hitherto", "hitherto")
            .expect("record_freq 失败");
    }
    let after = mix_candidates_with(cfg.clone(), Arc::clone(&store), "hi");
    assert!(
        after.contains(&"hitherto".to_string()),
        "学过词频的词应被提进列表，哪怕它在词库原序里排在配额之外（前 8: {:?}）",
        &after[..8.min(after.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// 配额一侧的对照：取数放大不得吃掉靠后成员的位置。
///
/// 上一条锁「窗口必须大」，这条锁「配额必须小」，缺了它 `truncate(MIX_MEMBER_QUOTA)` 被删时无人报警。
/// 本函数无跨成员排序、`seen` 先到先得，每个成员留多少就从靠后成员那里抢走多少位置，窗口放大会把
/// 末位的 `english` 成员整段推到翻页之外。
///
/// 判据取末位成员首条候选的下标上界而非「总条数 == 50」：后者随词库版本漂移变脆。
#[test]
fn member_quota_caps_each_member_segment() {
    if !has_schemas() || !data_dir().join("schemas/english").is_dir() {
        eprintln!("跳过：词库不存在");
        return;
    }
    // 出厂 quick_mix 的成员序：quick_input.* → $primary_pinyin → english，英文在末位。
    //
    // 码必须让靠前的拼音成员也吃满配额，否则前面没东西去挤后面、这条恒绿：`hi` 不是合法拼音音节，
    // 拼音成员零候选；`ni` 两边都有候选，才构成竞争。
    let (store, path) = fresh_store("wind_mix_quota_cap.redb");
    let cands = mix_candidates_with(mix_config(), Arc::clone(&store), "ni");

    // 英文候选＝纯 ASCII 那些（拼音成员给的是汉字）。取首个即英文成员段的起点。
    let first_english = cands
        .iter()
        .position(|c| c.is_ascii() && c.chars().any(|ch| ch.is_ascii_alphabetic()))
        .unwrap_or_else(|| {
            panic!(
                "前提：英文成员应给出候选（实际前 8: {:?}）",
                &cands[..8.min(cands.len())]
            )
        });
    assert!(
        first_english < MIX_MEMBER_QUOTA_UPPER_BOUND,
        "英文成员被靠前的拼音成员挤到了第 {first_english} 条——每成员配额没有生效，\
         末位成员会被推到翻页之外（共 {} 条）",
        cands.len()
    );
    let _ = std::fs::remove_file(&path);
}

/// 末位成员首条候选的可接受下标上界。取配额 + 一点余量（靠前成员可能不足配额、
/// 跨成员去重也会让实际位次略有浮动），而非精确的 50——精确值会随词库漂移变脆。
const MIX_MEMBER_QUOTA_UPPER_BOUND: usize = 60;
