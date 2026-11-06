//! 「1 个完整音节 + 尾部残码」的残码整句（step 2c 短上下文档 + step 6.5c 延迟定夺）。
//!
//! ## 这条路径要治的病
//!
//! 双拼打 `zdm`（zd=zai，m=「吗」的声母）首选是「在美国」而不是「在吗」。根因两层叠加：
//!
//! 1. **词库数据缺陷**：「在吗」在 `base.dict.yaml:103576` 里 `w=0`，而「在美国」
//!    在 ext 库里 `w=14796`；折扣算式严丝合缝 —— `14796 × 0.5² = 3699`、`0 × 0.5 = 0`。
//! 2. **层级硬闸门**：`w<=0` 命中 `demote_to_prefix_layer`，「在吗」被踢进前缀层，
//!    而 `cmp_match_layers` 的 `eff_prefix` 是层级键，**跨层不比权重** —— 第 98 位由此而来。
//!
//! 而第 2 条不能拆：它正压着 `zhonghuar` 的「种花人」(同样 w=0、distance=1)。
//! 「在吗」与「种花人」在 (weight, distance) 上**完全同形**，规则层面无从区分。
//! 出路是给「在吗」开第二条生成路径：残码整句由 Viterbi 用「在」+「吗」拼出，
//! 走单字乘积、**完全绕开词条 w=0**，这才第一次把两者分开。
//!
//! ## 与平台无关（曾被误判为安卓专属）
//!
//! 全拼 `zaim` 与双拼 `zdm` 的候选序**逐位相同**（连第 98 位都一样）—— 双拼转换后的
//! `query` 就是 `"zaim"`，两者在下游是同一个输入。PC 端一直有同样的问题，只是全拼用户
//! 会一路打完 `zaima`、很难在 `zaim` 停下；而双拼的 `zdm` 里 `zd` 已成完整音节，
//! **这是个自然的停顿点**。故本文件同时锁住两种方案。
//!
//! ## 为什么必须在协调器级测
//!
//! 引擎的 `sort_by` 不看 `consumed_length`，残码相关的顺序完全由 `candidate_display_order`
//! 决定（同 `pinyin_trailing_partial_order.rs` 的开篇说明）。引擎级测试对它没有约束力。
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

/// 反向闸门：短上下文残码整句**不得**制造噪音。
///
/// 放开 `syllables.len() >= 1` 会让每个「1 音节 + 残码」输入都多出一条整句，实测
/// `wom`→「我吗」、`tam`→「他吗」、`nim`→「你吗」、`meiy`→「没也」、`nih`→「你和」
/// 全部挤进第 2 位。故 step 6.5c 把这一档的插入**推迟到 step 4 之后**，按
/// `SENTENCE_KEEP_RATIO` 定夺，不够格的根本不进候选。
///
/// 本测试锁住这些输入的首选不被整句翻掉。谁把闸门放宽，这里当场变红。
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

/// 已完成音节 ≥ 2 的那一档**行为逐字不变**（`SENTENCE_KEEP_MAX_COMPLETED_SYLS` 的边界）。
///
/// 这些是 step 2c 原有的地盘，整句仍按 6.5b 让位给「恰好用完残码的补全」。
/// 与 `pinyin_trailing_partial_order.rs` 的断言重叠是有意的：那边锁的是 6.5b 本身，
/// 这边锁的是「新增的短上下文档没有溢出到 ≥2 这一侧」—— 两者失效方式不同。
///
/// ⚠️ `duibuq` 是这条边界的关键证据：它的**错解**「对不去」拿到 32.4× 的整句/补全比，
/// 比正解型的 `jisuanjik`「计算机看」(35.8×) 还高。一旦把
/// `SENTENCE_KEEP_MAX_COMPLETED_SYLS` 放宽到 2，「对不起」当场被顶掉。
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

/// **语法模型开启时同样成立** —— 这条是用户真机反馈换来的。
///
/// 闸门首版用「整句/补全的倍数」(24×) 做判据，而整句分数正是被语法模型平移的那一轴：
/// 开启万象(w=0.5)后 `zaim` 的「在吗」从 61.3× 掉到 14.0×，被自己的闸门拦掉，
/// 真机表现为「zdm 还是在美国」。判据遂改为**补全侧的绝对词频**
/// （[`COMPLETION_WEAK_CEILING`]，单轴、不受 grammar 影响）。
///
/// 谁再把判据换回依赖整句分数的形态，这里当场变红。
///
/// [`COMPLETION_WEAK_CEILING`]: 见 `wind_engine::pinyin` 的同名常量
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
/// 现有门禁只断言**首选**；本探针打印前 8，用来发现首选之外的暗伤
/// （`meiy` 的单字「没」正卡在 400 条上限的**第 400 位**，多产出一条候选就会把它挤掉
/// —— `engine_manager::test_pinyin_trailing_partial_prefix_floats_above_exact` 曾因此变红）。
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
