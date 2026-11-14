//! 码表简码位首选保护的端到端验证（设计见 docs/design/codetable-freq-short-code-protection.md）。
//!
//! 词库靠权重表达简码的钦定地位（`gen_dict` 给一简 9999 / 二简 9950 / 三简 9000），但
//! `freq_rerank::rerank_codetable_usedfirst` 的比较链**不含 weight**——只看「有没有被选过」。
//! 五笔一简 25 个码每个都是二选一（发行词库 `a` → 工 9999 / 戈 9998），次选字被误选一次
//! 就永久翻转（码表侧 used-first 不衰减）。本测试锁住「简码位保住钦定首选、全码位放开调频」。
//!
//! ## ⚠️ 三条用例必须合看，缺一即可能假绿
//!
//! - `short_code_head_survives_freq`：主用例，打 `a` 出「工」；
//! - `short_code_protection_off_lets_freq_win`：**反向对照**，同一份词频记录、只把保护关掉
//!   → 首选变「戈」。它同时证明了两件事：词频记录**确实进入了重排**（否则主用例的「工」
//!   只是因为词频压根没生效），以及 store key 的 schema/code 域写对了；
//! - `full_code_still_reranks`：全码位对照，证明保护是**分级**的而非全局硬保护。
//!
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**，
//! 判据是耗时（正常 2s 量级 vs 跳过 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
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

fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 建一个只含指定词频记录的 store（每个用例独立文件，避免相互污染）。
fn store_with(name: &str, hits: &[(&str, &str, u32)]) -> (Arc<wind_store::Store>, PathBuf) {
    let path = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    for (code, text, times) in hits {
        for _ in 0..*times {
            store
                .record_freq("wubi86", code, text)
                .expect("record_freq 失败");
        }
    }
    (store, path)
}

fn wubi_config(protect_short: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.frequency.enabled = true;
    cfg.schema.codetable.frequency.strategy = "step".into();
    if !protect_short {
        cfg.schema.codetable.frequency.protect_top_n_len1 = 0;
        cfg.schema.codetable.frequency.protect_top_n_len2 = 0;
    }
    cfg
}

/// 主用例：一简位的词库钦定首选不被词频顶掉。
#[test]
fn short_code_head_survives_freq() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    // 用户在 a 下选过 5 次次选字「戈」
    let (store, path) = store_with("wind_shortcode_protect_on.redb", &[("a", "戈", 5)]);
    let coord = Coordinator::new_headless_with_store(wubi_config(true), Some(&d), store);

    press(&coord, "a");
    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("工"),
        "一简位钦定首选须恒居首，实际候选: {head:?}"
    );
    assert!(
        all.iter().any(|t| t == "戈"),
        "保护的只是位次，次选字不得被赶出列表，实际候选: {head:?}"
    );

    let _ = std::fs::remove_file(&path);
}

/// **反向对照**：同一份词频记录，只把简码保护关掉 → 首选让给被用过的「戈」。
///
/// 这一条证明主用例的「工」不是因为词频压根没生效（store key 的 schema/code 域写错、
/// 调频开关没打开等都会让主用例假绿），而是保护真的挡住了它。
#[test]
fn short_code_protection_off_lets_freq_win() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let (store, path) = store_with("wind_shortcode_protect_off.redb", &[("a", "戈", 5)]);
    let coord = Coordinator::new_headless_with_store(wubi_config(false), Some(&d), store);

    press(&coord, "a");
    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("戈"),
        "关掉简码保护后词频须照常生效（否则主用例是假绿），实际候选: {head:?}"
    );

    let _ = std::fs::remove_file(&path);
}

/// 全码位对照：4 码位不设保护，用过的候选正常上浮。
/// 若把简码保护误做成全局硬保护，本用例会红。
///
/// 现场取自发行词库：`wgkq` → 使(2566) / 使唤(1137) / 覴(120)，前两条同为精确档。
/// （不用 `aaar`：其次选「菚」是生僻字，被常用字过滤挡在候选之外，测不出重排。）
///
/// ⚠️ **不可改用四叠码（`aaaa`/`cccc`…）当现场**：那 25 个码受 gen_dict 的
/// `[protected_codes]` 保护，权重固定在 8000+ 保护带、次序由上游钦定。拿它们做本用例，
/// 记频的那条本来就是首选，调频即便完全失效断言照样通过——**假绿**。
/// 本用例的鉴别力全靠「记频前它不是首选」这个前提。
#[test]
fn full_code_still_reranks() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let (store, path) = store_with("wind_shortcode_fullcode.redb", &[("wgkq", "使唤", 5)]);
    let coord = Coordinator::new_headless_with_store(wubi_config(true), Some(&d), store);

    press(&coord, "wgkq");
    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    // 前提自检：词库若变动到「使唤」本就排首位，本用例会退化成假绿，故先钉死前提。
    assert!(
        head.contains(&"使"),
        "前提失效——现场应同时有「使」与「使唤」，实际候选: {head:?}"
    );
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("使唤"),
        "全码位不设保护，用过的候选须正常上浮，实际候选: {head:?}"
    );

    let _ = std::fs::remove_file(&path);
}
