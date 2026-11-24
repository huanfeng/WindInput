//! 「1 个完整音节 + 尾部残码」的残码整句（step 2c 短上下文档 + step 6.5c 延迟定夺）。
//!
//! 要治的病：双拼 `zdm` / 全拼 `zaim` 首选是「在美国」而非「在吗」。「在吗」词条 w=0，被降到
//! 前缀层，而它与 `zhonghuar` 的「种花人」在 (weight, distance) 上同形、规则层无从区分；
//! 出路是让 Viterbi 用单字拼出残码整句，绕开词条 w=0。判据与取值依据见
//! docs/architecture/candidate-sorting-rules.md 的「拼音常量的取值依据」。
//!
//! 双拼转换后的 `query` 与全拼相同，两种方案同时锁。须在协调器级测：残码相关顺序由
//! `candidate_display_order` 决定，引擎的 `sort_by` 不看 `consumed_length`。
//!
//! 词典缺失时自动跳过。

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

fn config(schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 万象语法模型（420MB）是否可用 —— 不随安装包分发，缺失时相关用例自动跳过。
fn has_grammar() -> bool {
    data_dir()
        .join("schemas/pinyin/grammar/wanxiang-lts-zh-hans.gram")
        .exists()
}

fn config_with_grammar(schema: &str) -> Config {
    let mut cfg = config(schema);
    cfg.schema.pinyin.grammar.model = "wanxiang-lts-zh-hans.gram".into();
    cfg.schema.pinyin.grammar.weight = 0.5;
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

/// 敲入整串，返回协调器的全部候选文本（已按显示序排好）。
fn candidates_for(schema: &str, input: &str) -> Vec<String> {
    let coord = Coordinator::new_headless(config(schema), Some(&data_dir()));
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

/// 短上下文残码整句排首位：`zdm`/`zaim` → 「在吗」。
///
/// 全拼与双拼都要锁：两者在下游是同一个 `query`，任何一边掉了都说明改动碰到了共用路径。
/// `zdma`/`zaima`（打完，无残码）作对照 —— 它们本就正确，不该被本路径影响。
#[test]
fn short_context_sentence_takes_top() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (schema, input) in [
        ("shuangpin", "zdm"),
        ("pinyin", "zaim"),
        ("shuangpin", "zdma"),
        ("pinyin", "zaima"),
    ] {
        let cands = candidates_for(schema, input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some("在吗"),
            "{schema}/{input} 首选应为「在吗」（词库里它 w=0，只能靠残码整句救回），\
             实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
}

/// 反向闸门：短上下文残码整句不得制造噪音。
///
/// 立即插入会让 `wom`→「我吗」、`nim`→「你吗」等挤进前列；step 6.5c 推迟到 step 4 之后按
/// `COMPLETION_WEAK_CEILING` 定夺，不够格的不进候选。闸门放宽这里即变红。
#[test]
fn short_context_sentence_adds_no_noise() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [
        ("meiy", "没有"),
        ("nih", "你会"),
        ("wom", "我们"),
        ("tam", "他们"),
        ("nim", "你们"),
        ("zenm", "怎么"),
        ("shenm", "什么"),
        ("zhid", "知道"),
        ("yinw", "因为"),
    ] {
        let cands = candidates_for("pinyin", input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want),
            "{input} 首选应仍为「{want}」，残码整句不该翻上来，实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
}

/// 已完成音节 ≥ 2 的一档不受短上下文档影响（`SENTENCE_KEEP_MAX_COMPLETED_SYLS` 的边界），
/// 整句仍按 6.5b 让位给恰好用完残码的补全。
///
/// 与 `pinyin_trailing_partial_order.rs` 重叠是有意的：那边锁 6.5b 本身，这边锁短上下文档
/// 没有溢出到 ≥2 这一侧。
#[test]
fn long_context_still_yields_to_completion() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [
        ("beijingd", "北京的"),
        ("zhongguorenm", "中国人民"),
        ("nihaom", "你好吗"),
        ("duibuq", "对不起"),
    ] {
        let cands = candidates_for("pinyin", input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want),
            "{input} 首选应为「{want}」（已完成音节 ≥2，仍由 6.5b 让位给补全），\
             实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
}

/// 语法模型开启时同样成立。
///
/// 整句分数会被语法模型平移，故判据用补全侧的绝对词频（`wind_engine::pinyin` 的
/// `COMPLETION_WEAK_CEILING`）。判据换回依赖整句分数的形态（如整句/补全倍数）这里即变红。
#[test]
fn short_context_sentence_survives_grammar() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    if !has_grammar() {
        eprintln!("跳过：万象语法模型不存在（不随包分发）");
        return;
    }
    for (schema, input) in [("shuangpin", "zdm"), ("pinyin", "zaim")] {
        let cands = candidates_with(config_with_grammar(schema), input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some("在吗"),
            "{schema}/{input} 开着语法模型时首选也应为「在吗」，实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
    // 反向：开着语法模型也不许把噪音整句放上来
    for (input, want) in [("nih", "你会"), ("shih", "适合")] {
        let cands = candidates_with(config_with_grammar("pinyin"), input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want),
            "{input} 开着语法模型时首选应仍为「{want}」，实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
}

/// 残码场景的候选序快照，改判据前后各跑一次逐条比对。
///
/// 门禁只断言首选；本探针打印前 8，用来发现首选之外的暗伤（如 `meiy` 的单字「没」卡在
/// 400 条上限边缘，多产出一条候选就会被挤掉）。
#[test]
#[ignore = "定点探针：依赖 build_dev 真实词库，用 --ignored 显式运行"]
fn trailing_partial_order_snapshot() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (schema, input) in [
        ("shuangpin", "zdm"),
        ("pinyin", "zaim"),
        ("pinyin", "zhonghuar"),
        ("pinyin", "beijingd"),
        ("pinyin", "jisuanjik"),
        ("pinyin", "buzhidaok"),
        ("pinyin", "nihaom"),
        ("pinyin", "zhongguorenm"),
        ("pinyin", "beijingdaxuex"),
        ("pinyin", "nih"),
        ("pinyin", "meiy"),
        ("pinyin", "duibuq"),
    ] {
        println!(
            "{schema:>9} / {input:<14} → {:?}",
            candidates_for(schema, input)
                .iter()
                .take(8)
                .collect::<Vec<_>>()
        );
    }
}
