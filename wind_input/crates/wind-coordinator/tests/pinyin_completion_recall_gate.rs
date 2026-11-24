//! 词组补全的召回门槛（`completion_syllable_cap`）：`min_syllables` 决定「谁在场」。
//!
//! 「谁在前」由 `pinyin_completion_syllable_tier`（协调器 `cmp_completion_extra`）测，两侧分开
//! 以免互相牵连。那侧显式把 `min_syllables` 设回 2 制造跨档样本；本文件刻意吃出厂默认值。
//!
//! 出厂 4 对齐 librime `kNumSyllablesToPredictWord` 与 fcitx5 `LongWordLengthLimit`。它与
//! `max_extra_syllables` 是一对：上限 = `started < min ? started : started + max_extra`，
//! 4 + 5 = 9 恰好够「冰冻三尺非一日之寒」在打到第 4 个音节时进来。

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

/// 刻意不改 completion 配置：本文件测的就是出厂默认值的行为。
fn config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

fn candidates_for(input: &str) -> Vec<String> {
    let coord = Coordinator::new_headless(config(), Some(&data_dir()));
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

/// 出厂默认必须是 4 / 5：这是行为契约（4 对齐参考实现，5 由「4 + 5 = 9 音节」反推），改动须重审。
#[test]
fn factory_defaults_match_reference_implementations() {
    let c = Config::default().schema.pinyin.completion;
    assert_eq!(c.min_syllables, 4, "对齐 librime/fcitx5 的 4 音节预测门槛");
    assert_eq!(
        c.max_extra_syllables, 5,
        "与 min=4 配合给出 9 音节上限（冰冻三尺非一日之寒）"
    );
}

/// `started < min_syllables` ⇒ 上限收紧到 `started`：只出音节数对齐的候选。
///
/// `zaim` = zai + 残码 m ⇒ started = 2 < 4 ⇒ 3 音节的「在美国」「在没有」不该在场。
#[test]
fn below_min_syllables_yields_no_over_length_candidates() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let cands = candidates_for("zaim");
    assert!(!cands.is_empty(), "zaim 应有候选");

    for over in ["在美国", "在没有", "在哪里"] {
        assert!(
            !cands.contains(&over.to_string()),
            "started=2 < min=4，3 音节的「{over}」不该被召回；实际前 12: {:?}",
            &cands[..cands.len().min(12)]
        );
    }
    // 反向：音节数对齐的必须还在，否则「没有超音节候选」可能只是因为整个召回都空了。
    assert!(
        cands.iter().any(|t| t == "在吗" || t == "再买"),
        "2 音节候选应正常召回；实际前 12: {:?}",
        &cands[..cands.len().min(12)]
    );
}

/// `started >= min_syllables` ⇒ 上限放开到 `started + max_extra`，长词回来。
///
/// `bingdongsanch` = bing dong san + 残码 ch ⇒ started = 4，上限 4 + 5 = 9，
/// 恰好容得下 9 音节的「冰冻三尺非一日之寒」；`max_extra` 改小本条即红。
#[test]
fn at_min_syllables_long_word_is_recalled() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let cands = candidates_for("bingdongsanch");
    assert!(
        cands.iter().any(|t| t == "冰冻三尺非一日之寒"),
        "started=4 时 9 音节长词应被召回（上限 4+5=9）；实际前 12: {:?}",
        &cands[..cands.len().min(12)]
    );
}

/// 门槛之下的 2 音节输入仍给同音节数的词：`started < min` ⇒ 上限 = `started` 而非 0，
/// 只挡比输入长的。守的是文档站 `/docs/settings/schema/pinyin#completion` 对照表里的例子：
/// `dianh` → 电话/电荷、`nih` → 你好/你会。
#[test]
fn below_min_syllables_still_yields_same_length_words() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, wants) in [("dianh", ["电话", "电荷"]), ("nih", ["你好", "你会"])] {
        let cands = candidates_for(input);
        for w in wants {
            assert!(
                cands.contains(&w.to_string()),
                "{input}（started=2）应给出同为 2 音节的「{w}」——上限收紧到 started 本身，\
                 不是不给词组；实际前 12: {:?}",
                &cands[..cands.len().min(12)]
            );
        }
    }
}

/// 门槛之下的短输入：单音节照常只出单字（`d` 不出「但是」），给门槛调整一个下界锚点。
#[test]
fn single_syllable_still_yields_only_single_chars() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let cands = candidates_for("d");
    assert!(!cands.is_empty(), "d 应有候选");
    for word in ["但是", "的时候", "东西"] {
        assert!(
            !cands.contains(&word.to_string()),
            "单音节输入不该出词组「{word}」；实际前 12: {:?}",
            &cands[..cands.len().min(12)]
        );
    }
}
