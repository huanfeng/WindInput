//! 词组补全的**召回门槛**（`completion_syllable_cap`）：`min_syllables` 决定「谁在场」。
//!
//! ## 与 `pinyin_completion_syllable_tier` 的分工
//!
//! 两件事必须分开测，否则任一侧调整都要重写另一侧：
//!
//! | | 管什么 | 落点 |
//! |---|---|---|
//! | 本文件 | **谁在场**：超音节候选进不进候选列表 | 引擎召回 `completion_syllable_cap` |
//! | `..._syllable_tier` | **谁在前**：召回进来之后的先后 | 协调器 `cmp_completion_extra` |
//!
//! 那侧的用例显式把 `min_syllables` 设回 2 以制造跨档样本；本文件相反，**刻意吃出厂
//! 默认值**——它守的正是「出厂默认是多少」这件事本身。
//!
//! ## 出厂值 4 的由来
//!
//! 两个参考实现独立选定了同一个门槛：librime 的
//! `UserDictionary::kNumSyllablesToPredictWord = 4`、fcitx5-chinese-addons 的
//! `LongWordLengthLimit` 默认 4。语义一致：输入不足 4 个音节时不预测用户没打的内容。
//!
//! 用户诉求原话：「甚至在一些拼音输入法中，完全没有不符合音节数的候选。」——那正是
//! 本门槛在召回层做的事，排序档位做不到（它只能把超音节候选压后，压不掉「列表里全是
//! 我没打的音节」这个体感）。
//!
//! ## ⚠️ 这不是「越严越好」
//!
//! `min_syllables` 单独调大会把长词推迟到更晚才召回，故出厂值与 `max_extra_syllables`
//! 是一对：上限 = `started < min ? started : started + max_extra`。4 + 5 = 9 恰好够
//! 「冰冻三尺非一日之寒」在打到第 4 个音节时进来，最后一条用例钉的就是这个配合。

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

/// 刻意**不改** completion 配置：本文件测的就是出厂默认值的行为。
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

/// 出厂默认必须是 4 / 5，且两者配套。
///
/// 直接钉数值看着笨，但这两个值是**行为契约**而非实现细节：4 对齐参考实现，5 由
/// 「4 + 5 = 9 音节」这个具体需求反推。任一被改动都该有人重新读一遍上面的推导。
#[test]
fn factory_defaults_match_reference_implementations() {
    let c = Config::default().schema.pinyin.completion;
    assert_eq!(c.min_syllables, 4, "对齐 librime/fcitx5 的 4 音节预测门槛");
    assert_eq!(
        c.max_extra_syllables, 5,
        "与 min=4 配合给出 9 音节上限（冰冻三尺非一日之寒）"
    );
}

/// `started < min_syllables` ⇒ 上限收紧到 `started`：**只出音节数对齐的候选**。
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
/// 恰好容得下 9 音节的「冰冻三尺非一日之寒」。这条同时守住了 `max_extra` 的取值：
/// 若它被改回 3，上限只到 7，本条立刻变红。
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

/// 门槛之下的**2 音节**输入仍给同音节数的词 —— 收紧的是「超出部分」，不是整个词组。
///
/// 这条守的是**文档站上写着的例子**（`/docs/settings/schema/pinyin#completion` 的对照表）：
/// `dianh`(dian + 残码 h) → 电话/电荷、`nih`(ni + 残码 h) → 你好/你会。它们都是 2 音节，
/// `started` 也是 2，上限收紧到 `started` 本身时**恰好等于**、必须放行。
///
/// ⚠️ 出厂值从 2 提到 4 时很容易误以为「2 音节输入不再给词组」——不是。
/// `started < min` ⇒ 上限 = `started`，不是 0；被挡掉的只有**比输入长**的那些。
/// 文档若按前一种理解改写就会与实现脱节，故在此钉死。
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

/// 门槛之下的**短输入**不受影响：单音节照常只出单字，不混进词组。
///
/// 这是 `min_syllables` 从 1 提到 2 时就有的行为（`d` 不出「但是」），提到 4 之后
/// 覆盖面变宽但语义不变。放在这里是为了让「门槛提高」的回归有个下界锚点。
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
