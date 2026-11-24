//! 整句排序标记的跨层贯通测试
//!
//! `freq_rerank` 按 `Candidate::is_sentence` 族标记识别整句。标记要穿过
//! `finalize_candidates` → `build_candidates` → `apply_freq_rerank` 整条链路，任何一处
//! 重建 Candidate 都会静默丢掉它（不报编译错，只让排序走样）。
//!
//! 整句在 `freq_rerank` 里一律不锚定：锚定是硬闸门，会让同码词（「共和」压「恭贺」）
//! 的词频维度整体失效。下面三条用例守的是「整句不得压死词频」。
//!
//! 词典缺失时自动跳过。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    // 三级到仓库根（build_dev 在仓库根）。少写一级会解析到不存在的目录，
    // 整族测试静默走「跳过」分支假绿（判据：耗时 0.00s）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn key_event(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn press_letter(coord: &Coordinator, c: char) {
    let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
    coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN));
}

/// 「恭贺」被反复使用出高词频后，gonghe 的首选须让给它；整句「共和」退居第二而非消失。
///
/// 若整句在 `freq_rerank` 里恢复锚定，「共和」会把「恭贺」永久压在下面。
#[test]
fn test_sentence_flags_survive_to_freq_rerank() {
    let d = data_dir();
    if !d.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }

    let store_path = std::env::temp_dir().join("wind_sentence_flag_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // 「恭贺」积累可观使用次数（拼音衰减分远超 PINYIN_FREQ_EPSILON）
    for _ in 0..30 {
        store
            .record_freq("pinyin", "gonghe", "恭贺")
            .expect("record_freq 失败");
    }

    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&d), store);

    for c in "gonghe".chars() {
        press_letter(&coord, c);
    }

    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(5).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("恭贺"),
        "反复使用过的同码词须能反超整句（整句不得在 freq_rerank 里锚定），实际候选: {head:?}"
    );
    assert_eq!(
        all.get(1).map(|s| s.as_str()),
        Some("共和"),
        "整句只是退居第二、不得被赶出列表（本标记只摘锚定、不动 weight），实际候选: {head:?}"
    );

    let _ = std::fs::remove_file(&store_path);
}

/// 有词频记录时，未消费整串的整句不得靠锚定推翻按消费长度排好的序。
///
/// `buzhidaok`：整句「不知道」只消费 8/9 键，残码整句「不知道看」消费满 9/9、被协调器
/// 排在首位。`freq_rerank` 是最后一道整体排序，若「不知道」锚定就会把「不知道看」挤到第二。
#[test]
fn test_partial_consumption_sentence_does_not_anchor_over_full() {
    let d = data_dir();
    if !d.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let store_path = std::env::temp_dir().join("wind_partial_anchor_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    // 必须给本次候选里真实存在的词记账：`apply_freq_rerank_in` 只收候选自身 freq_code
    // 查到的记录，无关记录会让重排直接 return、本用例假绿。
    // 选「不知道看什么」：权重未过 `COMPLETION_FAR_WEIGHT_FLOOR`、不会被提升，
    // 只触发重排而不参与这组排序竞争。
    for _ in 0..3 {
        store
            .record_freq("pinyin", "buzhidaokanshenme", "不知道看什么")
            .unwrap();
    }

    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&d), store);
    for c in "buzhidaok".chars() {
        press_letter(&coord, c);
    }

    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(5).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("不知道看"),
        "首选须仍是消费满 9 键的残码整句，不得被只消费 8 键的「不知道」靠锚定顶掉，实际: {head:?}"
    );
    let _ = std::fs::remove_file(&store_path);
}

/// 残码整句不锚定：用户反复选过的词须能靠位置提升反超它。
///
/// 残码整句多猜了一个未输入的音节（`jisuanjik` → 「计算机看」），若它锚定，
/// 词频对整个残码编码失效。
///
/// 记账码必须用候选自己的码（`jisuanjikexue`）而非输入串（见 `Coordinator::freq_code_with`
/// 与 `every_record_selection_call_goes_through_freq_code`），否则写出孤儿键、测试假绿。
#[test]
fn test_speculative_sentence_yields_to_frequently_used_word() {
    let d = data_dir();
    if !d.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let store_path = std::env::temp_dir().join("wind_spec_sentence_test.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    for _ in 0..30 {
        store
            .record_freq("pinyin", "jisuanjikexue", "计算机科学")
            .unwrap();
    }

    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&d), store);
    for c in "jisuanjik".chars() {
        press_letter(&coord, c);
    }

    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(5).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("计算机科学"),
        "选过 30 次的词须能反超残码整句「计算机看」（后者不该锚定），实际: {head:?}"
    );
    assert!(
        all.iter().any(|t| t == "计算机看"),
        "残码整句只是退居其后、不得消失（本标记只摘锚定、不动 weight），实际: {head:?}"
    );
    let _ = std::fs::remove_file(&store_path);
}
