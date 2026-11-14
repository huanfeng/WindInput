//! 词频记录**不得改变**长词的召回位次（`cmp_by_consumed` 在两道排序里同口径）。
//!
//! ## 要守什么
//!
//! 真机报障：「冰冻三尺非一日之寒」打到 `bingdongsanchi` 首次能出（第 1 位），**上过一次
//! 屏、进了词频表之后就再也出不来**（掉到第 24 位），从词频表里删掉又恢复。
//!
//! 根因是两道排序的键不一致：协调器 `candidate_display_order` 以 `cmp_by_consumed` 开头，
//! 而最后一道整体排序 `freq_rerank::rerank_positional` 当时只有 `cmp_match_layers`；后者
//! **仅在本次输入有词频记录时才跑**（`recs.is_empty()` 直接 return）。该候选在整音节边界上
//! 拿不到残码上浮（`is_promoted_completion=false`）⇒ 落进前缀补全层 ⇒ 被 `bing` 的几十个
//! 同音单字整层压住，而它消费了整串、本该由 `cmp_by_consumed` 顶在最前。
//!
//! ⚠️ 该候选的词频记录**一格都没提升它**（`promote_prefix=single` 对 9 字候选判假），
//! 记录唯一的作用是触发那道用错键的重排 —— 所以这不是「词频排序不准」，是「词频的有无
//! 改变了与词频无关的次序」。
//!
//! ## 两条断言的分工
//!
//! 只测「位次不变」会被「把词频整个关掉」这种假修复骗过，故配一条**反向对照**：`zaim` 下
//! 给「在卖」记一次词频，它必须真的前移。两条一起才说明「重排跑了，且没有越权」。
//!
//! 对照取**同档内**的竞争（`zaim` 下「在卖」与「再买」同为 `extra = 0`），与音节数档位
//! （`cmp_completion_extra`）解耦：档位只在显示序施加一次、经 `base_pos` 传入本重排，
//! 拿跨档样本做对照会把两件事的回归绑在一起，任一侧调整都要重写这条。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

/// 词库里的 9 音节长词（`base.dict.yaml`：`冰冻三尺非一日之寒 bing dong san chi fei yi ri zhi han 115`）。
const WORD: &str = "冰冻三尺非一日之寒";
const CODE: &str = "bingdongsanchifeiyirizhihan";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_pinyin() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

fn config() -> Config {
    config_of("pinyin")
}

fn config_of(schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    // ⚠️ `Config::default()` 的 `frequency.enabled` 是 serde 的 `bool::default()` = **false**，
    // 与出厂 `data/config.toml` 的 `enabled = true` 不一致。不显式打开，本文件测的那道重排
    // 压根不会被调用 —— 整个文件会静默变成恒绿。
    cfg.schema.pinyin.frequency.enabled = true;
    // 取报障用户的实际设置（出厂现为 4 / 5，这里的 6 是他自己调宽的）。
    // 9 音节词要进得来：`started=4` 时上限 = 4 + max_extra。
    cfg.schema.pinyin.completion.min_syllables = 4;
    cfg.schema.pinyin.completion.max_extra_syllables = 6;
    cfg
}

/// 敲入整串，返回长词 [`WORD`] 在候选中的位次（不在则 `None`）。
fn rank_of_word(store: Arc<Store>, input: &str) -> Option<usize> {
    rank_of(store, input, WORD)
}

/// 敲入整串，返回指定文本在候选中的位次（不在则 `None`）。
fn rank_of(store: Arc<Store>, input: &str, want: &str) -> Option<usize> {
    let coord = Coordinator::new_headless_with_store(config(), Some(&data_dir()), store);
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
    coord
        .debug_all_candidate_texts()
        .iter()
        .position(|t| t == want)
}

/// 敲入整串，返回首候选。
fn top_of(store: Arc<Store>, input: &str) -> Option<String> {
    top_of_schema(store, "pinyin", input)
}

/// 同 [`top_of`]，但指定方案（双拼用例要走 `shuangpin`）。
fn top_of_schema(store: Arc<Store>, schema: &str, input: &str) -> Option<String> {
    let coord = Coordinator::new_headless_with_store(config_of(schema), Some(&data_dir()), store);
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
    coord.debug_all_candidate_texts().first().cloned()
}

/// 报障用户的双拼串：`bk ds sj ii fw yi ri | v`
/// = bing dong san chi fei yi ri + 残码 zh ⇒ `started = 8`，长词 9 音节 ⇒ `extra = 1`。
const SP_INPUT: &str = "bkdssjiifwyiriv";

/// 等价的全拼串（`started` 同为 8）。用来证明这**不是双拼特有的**：双拼只是让人更容易
/// 走到这个输入长度（8 个音节全拼要敲 22 键，双拼 15 键就到）。
const FP_INPUT: &str = "bingdongsanchifeiyiriz";

/// `extra = 1` 的远距离预测，**选过一次不得抢走首位**。
///
/// 这是「按预测距离折抵次数」（`completion_distance_discount`）的核心回归。
///
/// ## 与 `..._at_trailing_partial` 的分工
///
/// 那条是 `extra = 4`，被前一版的硬阈值 `extra <= 1` 挡住；本条恰好卡在阈值**里侧**，
/// 是同一个洞的另一半 —— 只测其一，把判据换成任何一个阈值都能全绿。
///
/// 位次减半模型下 `base_pos = 1` 的候选一次词频即到首位（刻意设计，拿微软拼音标定），
/// 所以这里没有「多留几次余量」的空间：`extra = 1` 折掉 1 次后必须恰好归零。
#[test]
fn one_freq_record_does_not_let_extra_one_completion_take_top() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (tag, schema, input) in [("sp", "shuangpin", SP_INPUT), ("fp", "pinyin", FP_INPUT)] {
        let store = fresh_store(&format!("extra_one_{tag}"));
        let before = top_of_schema(store.clone(), schema, input);

        store.record_freq("pinyin", CODE, WORD).expect("写词频");
        let after = top_of_schema(store.clone(), schema, input);

        assert_eq!(
            after, before,
            "{schema} {input}: 一次词频不得改变首候选（修复前 extra=1 的长词会抢到第 0 位）"
        );
        assert_ne!(
            after.as_deref(),
            Some(WORD),
            "{schema} {input}: 首候选应是音节数对齐的候选，不是还差 1 个音节的预测词"
        );
    }
}

fn fresh_store(tag: &str) -> Arc<Store> {
    let root = std::env::temp_dir().join(format!("wind_freq_longword_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"))
}

/// 整音节边界（`...chi`，无残码）：词频记录不得让长词沉底。
#[test]
fn freq_record_does_not_sink_long_word_at_syllable_boundary() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let store = fresh_store("boundary");
    let before = rank_of_word(store.clone(), "bingdongsanchi").expect("首次输入时长词应在候选中");

    store.record_freq("pinyin", CODE, WORD).expect("写词频");
    let after =
        rank_of_word(store.clone(), "bingdongsanchi").expect("有词频记录后长词仍应在候选中");

    assert_eq!(
        after, before,
        "词频记录不得改变长词位次（修复前：{before} → 24，被同音单字整层压住）"
    );
    // 位次本身也钉一下：只断言「相等」的话，两边一起烂掉（都在第 24 位）也能过。
    assert!(
        before <= 2,
        "长词消费了整串，应由 cmp_by_consumed 顶在最前，实际第 {before} 位"
    );
}

/// 残码位（`...chif`）：词频记录也不得让**远距离预测**的长词抢走首位。
///
/// 与上一条是同一个诉求（「上过屏与否体验一致」）的另一半，但根因不同，故分开测：
/// 上一条是排序键缺 `by_consumed`，这条是 `promotion_power` 对远距离预测太宽松 ——
/// 残码位上该词 `is_promoted_completion=true`，白拿了「免受 `promote_prefix` 限制」这份
/// 豁免（那份豁免本是给「补完手头这个音节」的近距离补全设的），于是**选过一次**就靠
/// `power=1` 把 `base_pos` 减半到 0，压过音节数恰好对齐的「冰冻三尺分」。
///
/// 判据见 `freq_rerank::completion_distance_discount`：`extra = 4` 要 5 次实证才起效。
/// 与 [`one_freq_record_does_not_let_extra_one_completion_take_top`]（`extra = 1`）
/// 一起，把折扣曲线的两端都钉住 —— 只留一端时，任何一个布尔阈值都能全绿。
#[test]
fn freq_record_does_not_let_far_completion_take_top_at_trailing_partial() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let store = fresh_store("far_partial");
    let before = top_of(store.clone(), "bingdongsanchif");

    store.record_freq("pinyin", CODE, WORD).expect("写词频");
    let after = top_of(store.clone(), "bingdongsanchif");

    assert_eq!(
        after, before,
        "词频记录不得改变首候选（修复前：「冰冻三尺分」→「{WORD}」）"
    );
    assert_ne!(
        after.as_deref(),
        Some(WORD),
        "首候选应是音节数对齐的候选，不是还差 4 个音节的远距离预测词"
    );
    // 降级不销毁：长词仍要在候选里（这是用户的另一条诉求——打到每个字的声母都该看得到它）。
    assert!(
        rank_of(store, "bingdongsanchif", WORD).is_some(),
        "长词应仍在候选中"
    );
}

/// 反向对照：**同档内**的词频提升确实在起作用（证明那道重排真的被调用了）。
///
/// 缺了这条，「把词频整个关掉」也能让上面那条通过。
///
/// 取 `zaim` 的「在卖」：它与「再买」同为 `extra = 0`（2 音节，对齐 started=2），
/// 档位一致 ⇒ 词频可以在档内调整先后。
#[test]
fn freq_record_still_promotes_within_same_syllable_tier() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let store = fresh_store("same_tier");
    let before = rank_of(store.clone(), "zaim", "在卖").expect("「在卖」应在 zaim 的候选中");

    store
        .record_freq("pinyin", "zaimai", "在卖")
        .expect("写词频");
    let after = rank_of(store.clone(), "zaim", "在卖").expect("有词频记录后仍应在候选中");

    assert!(
        after < before,
        "同档候选应能被词频前移；实际 {before} → {after}（重排没跑？）"
    );
}
