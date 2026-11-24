//! 前缀补全的音节边界对齐：合法音节输入下，补全只能从下一个音节开始。
//!
//! wdat key 是扁平码，前缀匹配发生在字符域，`shen` 会召回 `sheng` 的字（绕过模糊音开关
//! 白送 `en→eng`，还按 top-k 挤掉本音节低频字）。判据位取 `completed_len`（已成音节的字节长度）：
//! 不成音节的输入（`fe`）`completed_len == 0` 全放行，残码（`shenm`）落在残码之前、补全照常。
//! 背景与已接受的代价见 docs/architecture/candidate-sorting-rules.md 的「词库层前缀判据」。
//!
//! 与相邻两个文件分工（任一调整都不该动另两个）：本文件管「哪个音节」（补全是否停在音节边界）；
//! `pinyin_completion_recall_gate` 管「几个音节」（超音节候选进不进列表）；
//! `pinyin_completion_syllable_tier` 管「谁在前」（召回之后的显示次序）。

use std::collections::HashSet;
use std::path::PathBuf;
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

/// [`texts_of_code`] 要开的那个文件。
///
/// 它与 [`has_pinyin`] 查的不是一回事：那个是方案源（`cn_dicts/*.yaml`），这个是
/// `import_tables` 合并出的产物。测试进程未初始化 `CACHE_DIR`，产物才落在源文件旁边
/// （装机后在可写缓存目录，别照抄这个路径）。缺任一 `import_tables` 成员时合并不产出而源仍在。
fn merged_dict_path() -> PathBuf {
    data_dir().join("schemas/pinyin/rime_frost.dict.merged.wdat")
}

/// 音节归属探针的数据是否就绪。
///
/// 判据必须落在真正要打开的合并产物上；只查 [`has_pinyin`] 会在「源在、产物不在」时放行，
/// 测试在打开词库处炸。
///
/// 不要改成「打不开就回空集」：`cross_syllable_hits` 会恒回空，断言全部假绿。
fn has_syllable_probe() -> bool {
    merged_dict_path().exists()
}

/// 刻意**不改** completion 配置：本文件测的是出厂默认下的行为。
fn config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

fn candidates_with(cfg: Config, input: &str) -> Vec<String> {
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in input.chars() {
        let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
        coord.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
    coord.debug_all_candidate_texts()
}

fn candidates_for(input: &str) -> Vec<String> {
    candidates_with(config(), input)
}

/// 某个精确码下的全部汉字，用来判定候选的**真实音节归属**。
///
/// 不能只按「出现在更长音节的码下」判定：多音字（「溺」ni/niao）会被误报成缺陷，
/// 故调用方一律先排除本音节码下的字。
fn texts_of_code(code: &str) -> HashSet<String> {
    let p = merged_dict_path();
    let r = wind_dict::datformat::WdatReader::open(&p).expect("打开真实拼音词库");
    r.search(code).into_iter().map(|e| e.text).collect()
}

/// 只属于 `longer` 各码、不属于本音节 `own` 码的字（即「确定越界」的那批）。
fn cross_syllable_hits(cands: &[String], own: &str, longer: &[&str]) -> Vec<String> {
    let own_set = texts_of_code(own);
    let longer_set: HashSet<String> = longer.iter().flat_map(|c| texts_of_code(c)).collect();
    cands
        .iter()
        .filter(|t| !own_set.contains(*t) && longer_set.contains(*t))
        .cloned()
        .collect()
}

/// ★ 合法音节输入：**一条**更长音节的字都不该出现。
///
/// 三组样本覆盖三种韵尾延长形态：`n→ng`（报障原样）、以及 `i` 后接多种韵母。
#[test]
fn legal_syllable_excludes_longer_syllable_candidates() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    if !has_syllable_probe() {
        eprintln!(
            "跳过：音节归属探针词库不存在（{}）——import_tables 未全量合并",
            merged_dict_path().display()
        );
        return;
    }
    for (input, own, longer) in [
        ("fen", "fen", &["feng"][..]),
        ("shen", "shen", &["sheng"][..]),
        (
            "ni",
            "ni",
            &["nin", "ning", "niu", "nie", "nian", "niang", "niao"][..],
        ),
    ] {
        let cands = candidates_for(input);
        assert!(!cands.is_empty(), "{input} 应有候选");
        let bad = cross_syllable_hits(&cands, own, longer);
        assert!(
            bad.is_empty(),
            "{input} 是完整音节，不该出现更长音节（{longer:?}）的字；实际混入 {} 条：{:?}",
            bad.len(),
            &bad[..bad.len().min(12)]
        );
        // 反向：本音节的字必须还在，否则「没有跨音节候选」可能只是整个召回都空了。
        let own_set = texts_of_code(own);
        assert!(
            cands.iter().filter(|t| own_set.contains(*t)).count() >= 5,
            "{input} 本音节的字应正常召回；实际前 12: {:?}",
            &cands[..cands.len().min(12)]
        );
    }
}

/// 输入不成音节时前缀匹配必须照旧全开（判据的退化通道，不是漏网）。
///
/// 若这条也被收紧，`fe`/`zh`/`m` 这类输入会直接零候选。
#[test]
fn illegal_syllable_still_prefix_matches() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    if !has_syllable_probe() {
        eprintln!(
            "跳过：音节归属探针词库不存在（{}）——import_tables 未全量合并",
            merged_dict_path().display()
        );
        return;
    }
    let cands = candidates_for("fe");
    let hits = cross_syllable_hits(&cands, "fe", &["fen", "feng", "fei"]);
    assert!(
        hits.len() >= 20,
        "fe 不成音节，应按字符前缀召回 fen/feng/fei 的字；实际只有 {} 条：{:?}",
        hits.len(),
        &hits[..hits.len().min(12)]
    );
}

/// 关掉模糊音挡住 `feng`，开了就必须放回来。
///
/// en→eng 等价归模糊音层（带 `fuzzy_penalized` 惩罚），不该由前缀匹配白送。少了这条，
/// 把前缀匹配收紧成「模糊音也出不来」同样能让上面几条变绿。
#[test]
fn fuzzy_layer_still_provides_eng_when_enabled() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    if !has_syllable_probe() {
        eprintln!(
            "跳过：音节归属探针词库不存在（{}）——import_tables 未全量合并",
            merged_dict_path().display()
        );
        return;
    }
    let mut cfg = config();
    cfg.schema.pinyin.fuzzy.enabled = true;
    cfg.schema.pinyin.fuzzy.en_eng = true;
    let cands = candidates_with(cfg, "fen");
    let hits = cross_syllable_hits(&cands, "fen", &["feng"]);
    assert!(
        hits.len() >= 10,
        "开了 en_eng 模糊音后 feng 的字必须回来；实际只有 {} 条：{:?}",
        hits.len(),
        &hits[..hits.len().min(12)]
    );
}

/// 残码补全不受影响：判据位落在残码**之前**（`shenm` 取 `shen` 的长度 4）。
///
/// 若边界判据错用击键长度而非 `completed_len`，这些残码补全（见 `pinyin_completion.rs`）
/// 会集体消失。
///
/// 不配「残码位也要拦住跨音节」的反向用例：能延长当前音节的字符会被 `Dag::maximum_match`
/// 并进音节，「残码」与「跨越已完成音节」结构上互斥，那样的用例恒绿。
#[test]
fn trailing_partial_completion_intact() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [("shenm", "什么"), ("meiy", "没有"), ("nih", "你好")] {
        let cands = candidates_for(input);
        assert!(
            cands.contains(&want.to_string()),
            "{input} 应补出「{want}」（残码位的补全不受边界判据约束）；实际前 12: {:?}",
            &cands[..cands.len().min(12)]
        );
    }
}
