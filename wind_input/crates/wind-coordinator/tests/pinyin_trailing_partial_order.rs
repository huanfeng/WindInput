//! 尾部残码场景的**协调器级**候选序（`candidate_display_order` 的权威显示序）。
//!
//! 为什么必须在协调器级测：引擎的 `sort_by` **不看 `consumed_length`**（那边 `truncate`
//! 紧随排序，用消费长度当首要键会让消费更少的候选被整批丢弃而非排后）。残码相关的顺序
//! 完全由协调器决定，引擎级测试对它没有约束力 —— 本文件的每一条在引擎级都测不出来。
//!
//! 覆盖两件事：
//! 1. step 2c 残码补全整句（`buzhidaok`→不知道看）确实排到首位；
//! 2. 引擎侧**用 weight 表达的让位**（step 6.5b）能穿过协调器 —— 这条曾经断过，
//!    见 `test_engine_weight_yield_survives_coordinator` 的文档。
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
    // ⚠️ **必须显式设回 2**，不能吃出厂默认（现为 4）。
    //
    // 本文件测的是「让位，不是消失」——即超音节候选被排到后面但仍在场。而
    // `min_syllables` 管的是「在不在场」：`started < min` 时上限收紧到 `started`。
    // 出厂值升到 4 之后，`zhonghuar`（started=3）下的「中华人民」在**召回层**就没了，
    // 断言会以「让位变消失」失败——可它压根不是让位逻辑坏了，是样本没进来。
    //
    // 召回门槛的出厂行为由 `pinyin_completion_recall_gate` 守，本文件只管排序。
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

/// 残码整句排首位：`buzhidaok` → 「不知道看」。
///
/// 用户对照主流输入法提出的原始诉求：「就算没有词，也是通过智能组句强制生成，
/// 词库的更长词也要在第二三之类的位置」。故这里同时断言词库长词「不知道看什么」
/// **仍在候选中**（只是让位），不是被过滤掉了。
#[test]
fn test_trailing_partial_sentence_takes_top() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want, longer) in [
        ("buzhidaok", "不知道看", "不知道看什么"),
        ("jisuanjik", "计算机看", "计算机科学"),
        // `zhonghuar` 同时存在两条整句：残码整句「中华人」(消费 9 键) 与 step 2 整句
        // 「中华」(消费 8 键)。**它们的先后只由 ⓪ `consumed_length` 决定**，与 weight
        // 无关 —— 整句 weight 改几何平均后「中华」在**引擎级**反超（单词整句 vs 两词
        // 整句），而这里必须仍是「中华人」。引擎级那条断言已相应放宽为「首选是某条
        // 整句」，判据搬到本层，见 `pinyin_completion::low_freq_far_completion_*`。
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

/// 引擎侧**用 weight 表达的让位**必须能穿过协调器。
///
/// step 6.5b 让整句让位于「恰好用完残码的补全」，手法是把整句 weight 压到
/// `补全 weight - 1`（`nihaom` 下「你好们」82 < 「你好吗」83）。这个让位一度在协调器
/// 层失效：当时协调器的层级键另写了一份「忽略 `is_promoted_completion`」的副本，于是
/// 「你好吗」停在 `is_prefix` 层、被 `is_prefix=false` 的整句直接跨层压过 —— **布尔层级键
/// 等价于惩罚 ∞，weight 根本没机会说话**，首选变成「你好们」。
///
/// 修法是让协调器原样复用 `cmp_match_layers`。本测试锁住这条：谁再在协调器另写一份
/// 层级判据，这里当场变红。
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

/// 反向对照：**无残码**输入的首选不受 step 2c 影响。
///
/// 没有这一组，上面两条即使靠「把整句一律提到首位」之类的粗暴改法也能通过。
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
