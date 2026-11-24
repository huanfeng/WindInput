//! 音节数对齐者优先（`cmp_completion_extra`）的协调器级显示序：`zaim` 表达 2 个音节时，
//! 「在美国」「在没有」不得排在 2 字候选之前。
//!
//! 该档位只在协调器 `candidate_display_order` 施加一次，故必须在协调器级测。引擎 `sort_by`
//! 不加（紧随的 `truncate` 会把降级变成销毁）；`freq_rerank` 不重复加（会变成词频也翻不过的
//! 硬约束）。权重折扣 `COMPLETION_WEIGHT_DISCOUNT` 压不过词频差，是本档位存在的理由，
//! 详见 `wind_candidate::cmp_completion_extra` 与 docs/architecture/candidate-sorting-rules.md。

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

fn config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    // 必须显式设回 2，不能吃出厂默认（4）：出厂门槛下 `zaim`（started=2）的「在美国」在召回层
    // 就被挡掉，排序逻辑一次都执行不到。召回门槛由 `pinyin_completion_recall_gate` 守。
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
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

/// 报障三例：输入表达 2 个音节时，3 音节的补全不得排在 2 音节候选之前。
///
/// 断言写成「A 在 B 之前」而不是「A 在第 N 位」：位次会随词库版本漂移，先后关系才是被测的
/// 那条规则。三例的 `started` 都是 2（1 个完整音节 + 1 个残码）。
#[test]
fn syllable_aligned_candidates_precede_longer_completions() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    // (输入, 2 音节候选, 3 音节补全)
    for (input, aligned, longer) in [
        ("zaim", "再买", "在美国"),
        ("zaim", "在买", "在没有"),
        ("zaij", "再见", "再加上"),
        ("meiy", "每月", "每一个"),
    ] {
        let c = candidates_for(input);
        let pa = c.iter().position(|t| t == aligned);
        let pl = c.iter().position(|t| t == longer);
        let (Some(pa), Some(pl)) = (pa, pl) else {
            panic!(
                "{input}: 候选缺失 aligned={aligned}({pa:?}) longer={longer}({pl:?})，前 12: {:?}",
                &c[..12.min(c.len())]
            );
        };
        assert!(
            pa < pl,
            "{input}: 音节数对齐的「{aligned}」(第 {pa} 位) 应先于 3 音节补全「{longer}」(第 {pl} 位)"
        );
    }
}

/// 更长的补全只是降级、没有被销毁，翻页可及；「根本不出现」归 `[schema.pinyin.completion]` 的召回闸门。
#[test]
fn longer_completions_are_demoted_not_dropped() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, longer) in [("zaim", "在美国"), ("zaij", "再加上"), ("meiy", "每一个")] {
        let c = candidates_for(input);
        assert!(
            c.iter().any(|t| t == longer),
            "{input}: 「{longer}」应仍在候选中（降级不销毁），实际共 {} 条",
            c.len()
        );
    }
}

/// 既有定点不受影响：残码整句 / 恰好用完残码的补全仍在首位。
///
/// 它们 `extra = 0`，与本档位无冲突；缺了这条，「多音节候选一律压到最后」的过度实现也能通过上面两条。
#[test]
fn existing_trailing_partial_fixtures_unaffected() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [
        ("zaim", "在吗"),     // step 6.5c 短上下文残码整句
        ("nihaom", "你好吗"), // step 6.5b 恰好用完残码的补全
        ("beijingd", "北京的"),
        ("buzhidaok", "不知道看"), // step 2c 残码整句
    ] {
        let c = candidates_for(input);
        assert_eq!(
            c.first().map(String::as_str),
            Some(want),
            "{input}: 首选应仍是「{want}」，实际前 6: {:?}",
            &c[..6.min(c.len())]
        );
    }
}
