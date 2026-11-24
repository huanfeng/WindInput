//! 尾部残码场景的协调器级候选序（`candidate_display_order` 的权威显示序）。
//!
//! 引擎的 `sort_by` 不看 `consumed_length`（紧随的 `truncate` 会把消费更少的候选整批丢弃），
//! 残码相关的顺序完全由协调器决定，故必须在协调器级测。覆盖两件事：
//! 1. step 2c 残码补全整句（`buzhidaok`→不知道看）确实排到首位；
//! 2. 引擎侧用 weight 表达的让位（step 6.5b）能穿过协调器。
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

fn config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    // 必须显式设回 2，不能吃出厂默认（4）：出厂门槛下 `zhonghuar`（started=3）的「中华人民」
    // 在召回层就没了，「让位」断言会因样本缺失而红。召回门槛由 `pinyin_completion_recall_gate` 守。
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
    cfg
}

/// 敲入整串，返回协调器的全部候选文本（已按显示序排好）。
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

/// 残码整句排首位：`buzhidaok` → 「不知道看」；词库长词「不知道看什么」仍在候选中，只是让位。
#[test]
fn test_trailing_partial_sentence_takes_top() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want, longer) in [
        ("buzhidaok", "不知道看", "不知道看什么"),
        ("jisuanjik", "计算机看", "计算机科学"),
        // 残码整句「中华人」(消费 9 键) 与 step 2 整句「中华」(消费 8 键) 的先后只由
        // `consumed_length` 决定，与 weight 无关（引擎级「中华」可能反超，那侧只断言首选是整句）。
        ("zhonghuar", "中华人", "中华人民"),
    ] {
        let cands = candidates_for(input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want),
            "{input} 首选应为残码整句「{want}」，实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
        assert!(
            cands.iter().any(|c| c == longer),
            "词库长词「{longer}」应仍在候选中（让位，不是消失）"
        );
    }
}

/// 引擎侧用 weight 表达的让位必须能穿过协调器：step 6.5b 把整句 weight 压到 `补全 weight - 1`
/// （`nihaom` 下「你好们」<「你好吗」）。协调器须原样复用 `cmp_match_layers`；另写一份层级判据
/// 时布尔层级键会跨层压过 weight，首选变成「你好们」。
#[test]
fn test_engine_weight_yield_survives_coordinator() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [
        ("nihaom", "你好吗"),
        ("beijingd", "北京的"),
        ("zhongguorenm", "中国人民"),
    ] {
        let cands = candidates_for(input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want),
            "{input} 首选应为「{want}」（引擎已用 weight 让位给它），实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
}

/// 反向对照：无残码输入的首选不受 step 2c 影响，否则「把整句一律提到首位」也能通过上面两条。
#[test]
fn test_no_trailing_partial_unaffected() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [
        ("nihao", "你好"),
        ("woshi", "我是"),
        ("ceshi", "测试"),
        ("shurufa", "输入法"),
        ("zhongwen", "中文"),
        ("zhonghuarenmingongheguo", "中华人民共和国"),
    ] {
        let cands = candidates_for(input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want),
            "无残码输入 {input} 的首选不应改变，实际前 6: {:?}",
            cands.iter().take(6).collect::<Vec<_>>()
        );
    }
}
