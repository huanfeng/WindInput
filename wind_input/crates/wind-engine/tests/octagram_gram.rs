//! `OctagramGrammar` 对真实 `.gram` 的行为验证。
//!
//! 编码（CJK 压 2 字节）与 darts-clone 的位运算全是「错了也能跑、只是查不到」的那种代码，
//! 单测里的合成数据证明不了什么，必须拿真模型对已知搭配核对。
//!
//! ## 模型从哪来
//!
//! 默认找 `build_dev/data/schemas/pinyin/grammar/zh-hans-bgc.gram`，
//! 可用环境变量 `WIND_GRAM_PATH` 覆盖。获取方式：
//!
//! ```text
//! curl -L -o zh-hans-bgc.gram \
//!   https://github.com/lotem/rime-octagram-data/raw/hans/zh-hans-t-essay-bgc.gram
//! ```
//!
//! 该数据是 LGPL-3.0，故意不入库（`build_dev/` 已在 .gitignore）。
//! 分发方案见 `docs/design/language-model-integration.md` §5。

use std::path::PathBuf;

use wind_engine::pinyin::grammar::Grammar;
use wind_engine::pinyin::octagram::{OctagramConfig, OctagramGrammar};

/// 期望值来自独立实现（Python）对同一文件的解析，记在设计文档 §2.2.3。
/// 两套实现算出同一个数，才说明位域与编码都读对了。
const DE_SHIHOU_LN: f64 = 18.2669;
const YI_GE_LN: f64 = 19.6663;

fn gram_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WIND_GRAM_PATH") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../build_dev/data/schemas/pinyin/grammar/zh-hans-bgc.gram");
    p.exists().then_some(p)
}

/// 取模型；不存在时返回 None 并打印醒目的跳过说明，别让人以为跑过了（静默跳过会让计数照绿）。
fn open(weight: f64) -> Option<OctagramGrammar> {
    let Some(path) = gram_path() else {
        eprintln!(
            "!!! 跳过 octagram 测试：找不到 .gram 模型。\n\
             !!! 放到 build_dev/data/schemas/pinyin/grammar/zh-hans-bgc.gram \
             或设 WIND_GRAM_PATH。获取方式见本文件头部注释。"
        );
        return None;
    };
    let config = OctagramConfig {
        weight,
        ..Default::default()
    };
    Some(OctagramGrammar::open(&path, config).expect("打开 .gram 失败"))
}

#[test]
fn loads_real_gram_and_reports_units() {
    let Some(g) = open(0.0) else { return };
    // 2,599,424 个 unit（= 文件尾部字节数 / 4）
    assert!(
        g.unit_count() > 1_000_000,
        "unit 数异常: {}",
        g.unit_count()
    );
}

/// 命中时的零点，与 `OctagramConfig::default().baseline` 一致（ln 中位数）。
const BASELINE: f64 = 8.34;

/// 真实高频搭配必须查得到，且分值与独立实现逐位吻合。
///
/// `weight = 1` 时 `query = ln(频次) − baseline`，可从 query 反推 `ln(频次)` 与 Python 侧对账。
#[test]
fn known_collocations_match_independent_impl() {
    let Some(g) = open(1.0) else { return };
    let base = BASELINE;

    // 「的」+「时候」——bgc 是 2-gram，实际命中的是 的+时
    let got = g.query("的", "时候", false) + base;
    assert!(
        (got - DE_SHIHOU_LN).abs() < 1e-3,
        "的+时候: 期望 ln≈{DE_SHIHOU_LN}, 实得 {got}"
    );

    // 「一」+「个」
    let got = g.query("一", "个", false) + base;
    assert!(
        (got - YI_GE_LN).abs() < 1e-3,
        "一+个: 期望 ln≈{YI_GE_LN}, 实得 {got}"
    );
}

/// 未命中必须严格劣于任何命中（`−weight × baseline`）。
///
/// 让未命中返回 0（「中性」）会让没有搭配记录的碎片赢过有记录的正确词组：
/// 「建议修改」输给「见一修改」、「他的意思就是」输给「他的一死就是」。
#[test]
fn miss_is_strictly_worse_than_any_hit() {
    let Some(g) = open(1.0) else { return };
    let miss = g.query("鬻", "麤", false);
    assert!(
        (miss + BASELINE).abs() < 1e-9,
        "未命中应为 -weight*baseline, 实得 {miss}"
    );
    assert!(miss < 0.0, "未命中必须是负分，否则会奖励无记录的碎片");
}

/// 高频搭配得正分（零点在 `baseline`），且恒优于未命中。
#[test]
fn high_frequency_scores_positive() {
    let Some(g) = open(1.0) else { return };
    let hit = g.query("的", "时候", false);
    assert!(
        hit > 0.0,
        "ln≈{DE_SHIHOU_LN} 远高于 baseline={BASELINE}，应得正分，实得 {hit}"
    );
    assert!(hit > g.query("鬻", "麤", false), "命中必须优于未命中");
}

/// `weight = 0` 必须逐位等于「没挂模型」，保证能一键退回基线。
#[test]
fn zero_weight_is_exactly_neutral() {
    let Some(g) = open(0.0) else { return };
    for (ctx, word) in [("的", "时候"), ("一", "个"), ("", "你好"), ("鬻", "麤")] {
        assert_eq!(g.query(ctx, word, false), 0.0, "weight=0 必须恒返回 0");
        assert_eq!(g.query(ctx, word, true), 0.0, "句末同样");
    }
}

/// 句首（context 为空）没有上下文可用，等同未命中。
///
/// 这不只是边界情况：引擎侧没有 `preceding_text`（拿不到光标前已上屏的文本，见设计文档 §4.4），
/// 每句话的第一个词恒走这条路径。
#[test]
fn empty_context_falls_back_to_miss() {
    let Some(g) = open(1.0) else { return };
    assert!((g.query("", "你好", false) + BASELINE).abs() < 1e-9);
}

/// weight 是线性缩放：翻倍则偏离基线的幅度翻倍。
/// 标定时要靠这条把影响力调到合适量级。
#[test]
fn weight_scales_linearly() {
    let (Some(g1), Some(g2)) = (open(1.0), open(0.5)) else {
        return;
    };
    let a = g1.query("的", "时候", false);
    let b = g2.query("的", "时候", false);
    assert!((a - 2.0 * b).abs() < 1e-9, "weight 应线性缩放: {a} vs {b}");
}
