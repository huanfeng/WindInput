//! 整句排序标记的跨层贯通测试
//!
//! `freq_rerank` 的整句豁免此前靠 `weight >= 20_000_000` 判定，现改为按
//! `Candidate::is_sentence` 族语义标记。标记在引擎产出后要穿过
//! `finalize_candidates` → `build_candidates` → `apply_freq_rerank` 整条链路，
//! 任何一处重建 Candidate 都会把它丢掉——丢了不会编译报错，只会让整句排序
//! 静默走样。本测试锁住这条链路。
//!
//! ## ⚠️ 断言方向已随语义反转（原为「整句恒锚定首位」）
//!
//! 原测试断言 `gonghe` 的首选恒为整句「共和」，即便同码的「恭贺」有高词频。该语义
//! 已被推翻：「共和」自己就是一个词典精确整词，只是恰好被 Viterbi 选为最优解而继承
//! 了整句身份；锚定是**硬闸门**（`freq_rerank` 的 ① 步直接 return，衰减分连算都不算），
//! 于是同码的「恭贺」无论被选中多少次都翻不过它 —— 词频维度对整个 `gonghe` 编码失效。
//! `siyuan` 的「寺院」压住「思源」是同一现场（实测灌到 count=5000 仍纹丝不动）。
//!
//! 当时的修法是给这类「有同码竞争者」的整句标 `is_sentence_contested` 并摘掉其锚定。
//!
//! ## ⚠️ 锚定本身已移除，contested 已回收
//!
//! `freq_rerank` 的 `anchored` 只剩精确码短语，整句一律不锚定（整句 weight 与词库同量纲
//! 之后，硬闸门让「同量纲」只在无词频记录时成立）。于是 step 6.6 与
//! `is_sentence_contested`、`is_sentence_unanchored` 两个字段一并删除。
//!
//! **三条用例的目标不变**：它们锁的是「整句不得压死词频」，锚定移除后这个目标只会更强地
//! 成立，故断言一字未改 —— 现在它们守的是「整句不锚定」这条性质本身。
//!
//! **本测试保护的目标没有变**：`is_sentence` 与 `is_sentence_contested` 同为 Candidate 上
//! 相邻的 `serde(skip)` 字段，「跨层重建把标记丢掉」这一失效模式会同时丢掉两者 ——
//! 丢掉 contested 则锚定恢复、「共和」重回首位，本断言即会失败。判据方向反了，守的
//! 是同一条链路。
//!
//! 词典缺失时自动跳过。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    // 三级：crates/wind-coordinator → crates → wind_input → 仓库根（build_dev 在仓库根）。
    // 曾误写成两级，解析到 wind_input/build_dev/data —— 该目录不存在，于是下面的
    // exists() 判假、整个测试族静默走「跳过」分支通过。**判据是耗时 0.00s**。
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
/// 若整句在 `freq_rerank` 里恢复顶部锚定，「共和」会把「恭贺」永久压在下面——本断言
/// 即会失败（这正是最初的行为）。
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

/// 有词频记录时，**未消费整串的整句**不得靠锚定推翻按消费长度排好的序。
///
/// `buzhidaok`：step 2 整句「不知道」只消费 8/9 键（残码 `k` 没进去），step 2c 的残码整句
/// 「不知道看」消费满 9/9。协调器把后者排在首位（P0 的 `by_consumed`），但 `freq_rerank`
/// 是**最后一道整体排序**且锚定是**硬闸门** —— 只要存在任何一条词频记录（哪怕与这两个词
/// 都无关）重排就会触发，「不知道」若保持锚定就会把「不知道看」挤到第二。
///
/// ⚠️ 这条**与 step 2c 无关地早已存在**，只是在残码整句出现之前没有「消费更多的候选」
/// 来暴露它。现由「整句一律不锚定」直接保证，协调器不再按 `consumes_all` 标摘锚定位。
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
    // ⚠️ 必须给**本次候选里真实存在**的词记账，否则 freq_rerank 压根不会跑，本用例假绿。
    // `recs` 只收「候选自身 freq_code 查到的」记录（见 `apply_freq_rerank_in` 的 probe 收集），
    // 无关记录进不了 recs，`recs.is_empty()` 便直接 return —— 初版正是写了条无关记录，
    // 回退被测代码后测试照样绿。
    //
    // 选「不知道看什么」：它 w=32 未过 `COMPLETION_FAR_WEIGHT_FLOOR`(100) 故未上浮
    // （`is_promoted_completion=false`），于是被默认的 `promote_prefix=single` 挡在提升之外
    // （power=0），**只触发重排、不参与排序竞争**，恰好不干扰本用例要测的那组对比。
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

/// **残码整句不锚定**：用户反复选过的词须能靠位置提升反超它。
///
/// 残码整句多猜了一个用户还没输入的音节（`jisuanjik` → 「计算机**看**」），置信度低一档，
/// 而残码场景恰是最需要词频学习的地方。若它锚定，词频维度对整个残码编码整体失效——
/// 这与 `siyuan` 寺院/思源、`gonghe` 共和/恭贺是同一个现场（见本文件顶部说明）。
///
/// ⚠️ **记账码必须用候选自己的码**（`jisuanjikexue`），不是输入串：拼音走 `freq_code` 的
/// 候选码口径（见 `Coordinator::freq_code_with` 与守门测试
/// `every_record_selection_call_goes_through_freq_code`）。用输入串会写出一个孤儿键，
/// 读取端永远查不到，测试于是假绿——作者本人在写这条时就先踩了一次。
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
