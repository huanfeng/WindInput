//! 前缀补全的**音节边界对齐**：合法音节输入下，补全只能从下一个音节开始。
//!
//! ## 报障与根因（2026-09-04）
//!
//! 用户报「`[schema.pinyin.fuzzy] en_eng = false` 时打 `shen` 仍出 `sheng` 的字」。
//!
//! 根因不在模糊音，在**扁平码前缀匹配**：wdat 的 key 是 `nihao` 而非 `ni hao`（带空格的
//! key 会让 `niha` 无法前缀匹配，而逐键前缀匹配是「边打边出候选」的命脉），于是前缀匹配
//! 发生在**字符**域 —— `shen` 是 `sheng` 的字符前缀。而当时唯一的过滤器
//! `prefix_syllable_keep` 只数**音节个数**，`sheng` 和 `shen` 都是 1 音节，它分不开。
//!
//! 修复前实测（真实词库，出厂配置）：
//!
//! | 输入 | 候选总数 | 跨音节条目 | 最早位次 |
//! |---|---|---|---|
//! | `fen` | 56 | 27（全是 `feng`） | 第 30 位 |
//! | `shen` | 68 | 12（全是 `sheng`） | 第 49 位 |
//! | `ni` | 92 | 26（nin/ning/niu/nie/nian/niang/niao） | 第 35 位 |
//!
//! 危害有两重：多出不该有的，以及按 top-k **挤掉**本音节的低频字（`sheng` 的「生」
//! w=8631 高过 `shen` 的「深」w=5042）。波及面远超 en/eng 一组，`an/ang`、`in/ing`、
//! `ian/iang`、`uan/uang`、`a→ai/an/ao`、`e→ei/en/er`、`o→ou/ong` 全部同病。
//!
//! ## ★ 判据不是「关掉前缀匹配」，是「按 completed_len 对齐」
//!
//! 用户给出的参照行为（其他全拼输入法）：`fe` 不是合法拼音 ⇒ 用前缀匹配；打到 `fen`
//! 其本身是合法音节 ⇒ 按音节处理，不含 `feng`。
//!
//! 这两种形态由**同一个判据**覆盖，无需分支：判据位取 `completed_len`（输入中已构成
//! 完整音节那一段的字节长度），`fe` 切不出音节 ⇒ `completed_len == 0` ⇒ 位 0 恒是音节
//! 起点 ⇒ 全放行。残码（`shenm`）同理落在残码**之前**，补全照常。
//!
//! ## 与相邻两个文件的分工（三者任一调整都不该动另两个）
//!
//! | 文件 | 管什么 |
//! |---|---|
//! | 本文件 | **哪个音节**：补全是否停在音节边界上 |
//! | `pinyin_completion_recall_gate` | **几个音节**：超音节候选进不进候选列表 |
//! | `pinyin_completion_syllable_tier` | **谁在前**：召回之后的显示次序 |

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
        });
    }
    coord.debug_all_candidate_texts()
}

fn candidates_for(input: &str) -> Vec<String> {
    candidates_with(config(), input)
}

/// 某个精确码下的全部汉字，用来判定候选的**真实音节归属**。
///
/// ⚠️ **不能只按「出现在更长音节的码下」判定**：多音字（「溺」ni/niao 两读）在两个码下
/// 都存在，那样会把合法候选误报成缺陷。首版探针就是这么误报的（`ni` 的最早跨音节位次
/// 被算成第 11 位，剔除多音字后实为第 35 位）。故调用方一律先排除本音节码下的字。
fn texts_of_code(code: &str) -> HashSet<String> {
    let p = data_dir().join("schemas/pinyin/rime_frost.dict.merged.wdat");
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

/// ★ 输入**不成音节**时前缀匹配必须照旧全开 —— 这是判据的退化通道，不是漏网。
///
/// `fe` 不是合法拼音，用户显然还没打完，此时唯一能做的就是按字符前缀猜。若这条也被
/// 收紧，`fe`/`zh`/`m` 这类输入会直接零候选。
#[test]
fn illegal_syllable_still_prefix_matches() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
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

/// ★★ 关掉模糊音挡住 `feng`，**开**了就必须放回来。
///
/// 这条守的是「谁该提供 en→eng 等价」这个职责划分：它属于模糊音层（带
/// `fuzzy_penalized` 惩罚、有独立匹配层级），而不该由扁平码前缀匹配无条件白送。
/// 少了这条断言，把前缀匹配收紧成「模糊音也出不来」同样能让上面几条变绿。
#[test]
fn fuzzy_layer_still_provides_eng_when_enabled() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
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
/// 这几个定点是残码上浮机制存在的理由（见 `pinyin_completion.rs`），若边界判据错用了
/// 击键长度而非 `completed_len`，它们会集体消失。
///
/// ## ★ 为什么没有配一条「残码位也要拦住跨音节」的反向用例
///
/// 因为那件事**在结构上不可能发生**，写出来会是一条恒绿、抓不住任何回归的假测试
/// （初版真写了一条 `shenm` 不出「生命」，在**修改前的基线上同样是绿的**才发现）。
///
/// 原因：`shengming` 的第 4 个字符是 `g`、`shenm` 的是 `m`，它压根不是前缀。推而广之
/// —— 能延长当前音节的字符（`n`/`g`/韵母）一旦跟在后面，`Dag::maximum_match` 就会把它
/// 并进来切成更长的完整音节（`fen` + `g` 直接切成 `feng`），于是它**不再是残码**。
/// 所以「残码 + 跨越已完成音节」这两个条件互斥，判据在残码位恒放行是结构性的，
/// 不是特意开的口子。
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
