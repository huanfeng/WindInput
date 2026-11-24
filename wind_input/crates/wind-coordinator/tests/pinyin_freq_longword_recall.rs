//! 词频记录不得改变长词的召回位次：`freq_rerank::rerank_positional` 只在本次输入有词频记录时
//! 才跑，它的排序键必须与协调器 `candidate_display_order` 同样以 `cmp_by_consumed` 开头，
//! 否则「词频的有无」会改变与词频无关的次序（见 candidate-sorting-rules.md 的反例）。
//!
//! 反向对照（`zaim` 下给「在卖」记一次词频，它须真的前移）防「把词频整个关掉」的假修复。
//! 对照取同档内的竞争（「在卖」「再买」同为 `extra = 0`），与音节数档位 `cmp_completion_extra` 解耦。

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
    // `Config::default()` 的 `frequency.enabled` 是 false（出厂 `data/config.toml` 是 true），
    // 不显式打开则那道重排不会被调用，整个文件静默恒绿。
    cfg.schema.pinyin.frequency.enabled = true;
    // 刻意调宽到 6（出厂 4 / 5）：9 音节词要进得来，`started=4` 时上限 = 4 + max_extra。
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

/// 双拼串：`bk ds sj ii fw yi ri | v`
/// = bing dong san chi fei yi ri + 残码 zh ⇒ `started = 8`，长词 9 音节 ⇒ `extra = 1`。
const SP_INPUT: &str = "bkdssjiifwyiriv";

/// 等价的全拼串（`started` 同为 8），证明这不是双拼特有的。
const FP_INPUT: &str = "bingdongsanchifeiyiriz";

/// `extra = 1` 的远距离预测选过一次不得抢走首位（`completion_distance_discount`）。与
/// `..._at_trailing_partial`（`extra = 4`）一起钉住折扣曲线两端，只测一端时任何布尔阈值都能全绿。
/// 位次减半模型下 `base_pos = 1` 一次词频即到首位，故 `extra = 1` 折掉 1 次后须恰好归零。
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

/// 残码位（`...chif`）：词频记录也不得让远距离预测的长词抢走首位。残码位上该词
/// `is_promoted_completion=true`，免受 `promote_prefix` 限制，须靠
/// `freq_rerank::completion_distance_discount` 折抵（`extra = 4` 要 5 次实证才起效），
/// 否则选过一次就压过音节数对齐的「冰冻三尺分」。另一端见
/// [`one_freq_record_does_not_let_extra_one_completion_take_top`]。
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
    // 降级不销毁：长词仍要在候选里。
    assert!(
        rank_of(store, "bingdongsanchif", WORD).is_some(),
        "长词应仍在候选中"
    );
}

/// 反向对照：同档内的词频提升确实起作用（证明重排真的被调用了），否则「把词频整个关掉」也能通过上面几条。
/// 「在卖」与「再买」同为 `extra = 0`，词频可以在档内调整先后。
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
