//! 拼音方案英文候选的界面位次（`schema.pinyin.english_merge`），真实词库、协调器界面序：
//!
//! - 首选是吃满整串的高频拼音词库词（含简拼解出来的 bus→不是）时英文让到第 2；词库冷僻词
//!   （test→托尔斯泰）、拼凑句不算；这个英文词选过一次就不再让；
//! - `exact_position` 出厂按方案自动：全拼 1、双拼 2；
//! - 中文侧为空的前缀兜底页里，与输入相同的词排最前。
//!
//! 词库缺失时跳过（worktree 里要自备 `build_dev/data`）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready(schema: &str) -> bool {
    let d = data_dir();
    let ok = d.join(format!("schemas/{schema}.schema.toml")).exists()
        && d.join("schemas/english/en.dict.yaml").exists();
    if !ok {
        eprintln!("跳过：缺少 {schema} 方案或英文词库");
    }
    ok
}

fn key(key_code: u32) -> KeyEventData {
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

fn config(schema: &str, f: impl FnOnce(&mut Config)) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.english_merge.enable = true;
    f(&mut cfg);
    cfg
}

/// 打 `input`，返回协调器排好的全部候选文本（界面序）。
fn typed(cfg: Config, input: &str, tag: &str) -> Vec<String> {
    let path = std::env::temp_dir().join(format!("wind_py_en_policy_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for c in input.chars() {
        coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
    }
    coord.debug_all_candidate_texts()
}

fn index_of(list: &[String], word: &str, input: &str) -> usize {
    list.iter().position(|t| t == word).unwrap_or_else(|| {
        panic!(
            "{input} 应有候选 {word}，实际 {:?}",
            &list[..list.len().min(12)]
        )
    })
}

/// 简拼真词吃满整串（bus→不是、rug→如果）时，英文让到它后面。
#[test]
fn abbrev_real_word_leads_english() {
    if !ready("pinyin") {
        return;
    }
    for (input, word) in [("bus", "不是"), ("rug", "如果")] {
        let list = typed(config("pinyin", |_| {}), input, input);
        assert_eq!(
            list[0],
            word,
            "{input} 首选应是「{word}」，实际 {:?}",
            &list[..4]
        );
        assert_eq!(
            list[1],
            input,
            "{input} 英文应在第 2，实际 {:?}",
            &list[..4]
        );
    }
}

/// 首选只是词库冷僻词时英文仍第 1（test→托尔斯泰、app→挨批评、old→欧蓝德、web→玩儿吧）。
#[test]
fn rare_head_word_does_not_outrank_english() {
    if !ready("pinyin") {
        return;
    }
    for (input, rare) in [
        ("test", "托尔斯泰"),
        ("app", "挨批评"),
        ("old", "欧蓝德"),
        ("web", "玩儿吧"),
        ("big", "逼格"),
        ("fun", "妇女"),
    ] {
        let list = typed(config("pinyin", |_| {}), input, &format!("rare_{input}"));
        assert_eq!(list[0], input, "{input} 英文应第 1，实际 {:?}", &list[..4]);
        // 前提：首选位让出来的确是那条冷僻整串词——词库变了它不在第 2，本例就测不到门槛。
        assert_eq!(
            list[1],
            rare,
            "前提：{input} 第 2 应是「{rare}」，实际 {:?}",
            &list[..4]
        );
    }
}

/// 选过即免：bus 首次英文第 2，选一次英文 bus 之后再打，英文第 1。
///
/// 两种出厂相关的记频口都要认：拼音调频开（`data/config.toml` 出厂开，`Config::default()`
/// 是关的，故显式打开）记进拼音桶；只开英文调频时记进英文桶。
#[test]
fn english_selected_once_skips_yield() {
    if !ready("pinyin") {
        return;
    }
    let pinyin_bucket = |c: &mut Config| c.schema.pinyin.frequency.enabled = true;
    let english_bucket = |c: &mut Config| c.schema.english.frequency.enabled = true;
    for (tag, cfg) in [
        ("py", config("pinyin", pinyin_bucket)),
        ("en", config("pinyin", english_bucket)),
    ] {
        let path = std::env::temp_dir().join(format!("wind_py_en_policy_used_{tag}.redb"));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
        let type_bus = || {
            for c in "bus".chars() {
                coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
            }
            coord.debug_all_candidate_texts()
        };
        let first = type_bus();
        assert_eq!(
            first[1],
            "bus",
            "[{tag}] 前提：首次英文第 2，实际 {:?}",
            &first[..4]
        );
        coord.handle_key_event(&key(0x32)); // 数字键 2 选英文 bus
        assert!(
            coord.debug_input_buffer().is_empty(),
            "[{tag}] 选词后缓冲应清空"
        );
        let again = type_bus();
        assert_eq!(
            again[0],
            "bus",
            "[{tag}] 选过之后英文应第 1，实际 {:?}",
            &again[..4]
        );
    }
}

/// 选过即免只认与输入同码的英文完整单词：选过补全出来的 business，再打 bus 仍让「不是」。
#[test]
fn selected_completion_does_not_exempt() {
    if !ready("pinyin") {
        return;
    }
    let cfg = config("pinyin", |c| {
        c.schema.pinyin.frequency.enabled = true;
        c.schema.pinyin.english_merge.complete_min_length = 3;
    });
    let path = std::env::temp_dir().join("wind_py_en_policy_completion_used.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    let type_in = |s: &str| {
        for c in s.chars() {
            coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
        }
    };
    // 在 busines 下选中 business（中文侧为空，它在兜底补全页上）。
    type_in("busines");
    let page = coord.debug_page_texts();
    let i = page
        .iter()
        .position(|t| t == "business")
        .unwrap_or_else(|| panic!("前提：busines 首页应有 business，实际 {page:?}"));
    coord.handle_key_event(&key(0x31 + i as u32));
    assert!(coord.debug_input_buffer().is_empty(), "选词后缓冲应清空");
    type_in("bus");
    let again = coord.debug_all_candidate_texts();
    assert!(
        again.iter().any(|t| t == "business"),
        "前提：business 仍在列表里（否则测不到它会不会被当成选过 bus），实际 {:?}",
        &again[..6]
    );
    assert_eq!(
        again[0],
        "不是",
        "选过补全不算选过 bus，实际 {:?}",
        &again[..4]
    );
}

/// 拼凑句不算开头段：hello / world 英文仍第 1；hen 的英文位次不受影响。
#[test]
fn synthesized_sentence_still_yields_to_english() {
    if !ready("pinyin") {
        return;
    }
    for input in ["hello", "world"] {
        let list = typed(config("pinyin", |_| {}), input, input);
        assert_eq!(
            list[0],
            input,
            "{input} 英文应仍第 1，实际 {:?}",
            &list[..4]
        );
    }
    let hen = typed(config("pinyin", |_| {}), "hen", "hen");
    let i = index_of(&hen, "hen", "hen");
    // 开头只有常用精确单字（很 恨 狠 痕…）能排在它前面。
    assert!(
        hen[..i].iter().all(|t| t.chars().count() == 1),
        "hen 前面只该有精确单字，实际 {:?}",
        &hen[..=i]
    );
}

/// 双拼出厂下英文不当首选、排第 2；全拼 hello 仍第 1。
#[test]
fn shuangpin_english_not_first_by_default() {
    if !ready("shuangpin") || !ready("pinyin") {
        return;
    }
    for input in ["the", "good", "this"] {
        let list = typed(config("shuangpin", |_| {}), input, &format!("sp_{input}"));
        assert_ne!(
            list[0],
            input,
            "双拼 {input} 首选不该是英文，实际 {:?}",
            &list[..4]
        );
        assert_eq!(
            list[1],
            input,
            "双拼 {input} 英文应在第 2，实际 {:?}",
            &list[..4]
        );
    }
    let hello = typed(config("pinyin", |_| {}), "hello", "qp_hello");
    assert_eq!(hello[0], "hello");
}

/// 显式写了 `exact_position` 时全拼双拼都用它。
#[test]
fn explicit_exact_position_applies_to_both() {
    if !ready("shuangpin") || !ready("pinyin") {
        return;
    }
    let set3 = |c: &mut Config| c.schema.pinyin.english_merge.exact_position = 3;
    let sp = typed(config("shuangpin", set3), "the", "sp_the_3");
    assert_eq!(sp[2], "the", "双拼显式 3 时英文在第 3，实际 {:?}", &sp[..4]);
    // 设 1 只去掉双拼那一位的下限：首选「他和」是够格的高频词，英文照样排它后面。
    let set1 = |c: &mut Config| c.schema.pinyin.english_merge.exact_position = 1;
    let sp1 = typed(config("shuangpin", set1), "the", "sp_the_1");
    assert_eq!(&sp1[..2], ["他和", "the"], "实际 {:?}", &sp1[..4]);
    let qp = typed(config("pinyin", set3), "hello", "qp_hello_3");
    assert_eq!(
        qp[2],
        "hello",
        "全拼显式 3 时英文在第 3，实际 {:?}",
        &qp[..4]
    );
}

/// 中文侧为空的前缀兜底页：与输入相同的词排最前（hi 打出 hi 第 1）。
#[test]
fn prefix_fallback_puts_exact_word_first() {
    if !ready("pinyin") {
        return;
    }
    let list = typed(config("pinyin", |_| {}), "hi", "hi");
    assert_eq!(list[0], "hi", "hi 应排第 1，实际 {list:?}");
}

/// `complete_min_length` 开着、`max_count` 出厂 2：world 只多补 1 条，且补全不当首选。
#[test]
fn completion_capped_by_max_count() {
    if !ready("pinyin") {
        return;
    }
    let cfg = config("pinyin", |c| {
        c.schema.pinyin.english_merge.complete_min_length = 5
    });
    let list = typed(cfg, "world", "world_complete");
    assert_eq!(list[0], "world", "实际 {:?}", &list[..4]);
    let english: Vec<&String> = list
        .iter()
        .filter(|t| t.to_ascii_lowercase().starts_with("world"))
        .collect();
    assert_eq!(
        english.len(),
        2,
        "完整单词 + 补全合计 2 条，实际 {english:?}"
    );
}
