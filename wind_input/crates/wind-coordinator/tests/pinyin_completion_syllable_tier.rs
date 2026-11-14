//! **音节数对齐者优先**（`cmp_completion_extra`）的协调器级显示序。
//!
//! 用户诉求：「拼音的候选优先级应该优先匹配拼音音节数。`zaim` 算 2 个音节，应该优先出
//! 2 个字的，但现在『在美国』『在没有』之类却会很前。」
//!
//! ## 为什么必须在协调器级测
//!
//! 该档位**只在协调器 `candidate_display_order` 施加一次**，两处刻意不加：
//! - 引擎的 `sort_by`：那里 `truncate` 紧随排序，排序键同时决定**去留**，加进去等于
//!   「音节数超出即被截断」——销毁而非降级。分工与 `consumed_length` 完全一致。
//! - `freq_rerank` 的层级比较器：档位已随 `base_pos` 传进去了，再加一遍会把它升级成
//!   **词频也翻不过的硬约束**（`jisuanjik` 下选过 30 次的「计算机科学」会压不过
//!   「计算机看」，`pinyin_sentence_flag` 抓得到）。
//!
//! ⇒ 引擎级测试对本文件的每一条都没有约束力。
//!
//! ## 为什么权重折扣不够（这是本档位存在的理由）
//!
//! 引擎侧早有 `COMPLETION_WEIGHT_DISCOUNT`（`0.5^extra`），但它在对数域只有 0.69，
//! 压不过跨 5 个数量级的词频差：真实词库实测 `zaij` 的「再加上」(3 音节，折后 w=5278)
//! 稳压「再加」(2 音节，1461)、「再见」(1419)。详见 `wind_candidate::cmp_completion_extra`。

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
    // 本文件测的是「谁排在前」（`cmp_completion_extra` 显示序档位），而 `min_syllables`
    // 管的是「谁在场」（召回门槛）：`started < min` 时上限收紧到 `started`，超音节候选
    // 压根不进候选列表。出厂值升到 4 之后，`zaim`（started=2）下「在美国」在召回层就被
    // 挡掉了——两条断言会以「候选缺失」失败，而它们本想验的排序逻辑一次都没执行到。
    //
    // 两件事必须解耦：召回门槛的行为由 `pinyin_completion_recall_gate` 那侧的用例守，
    // 本文件只负责「召回进来之后的先后」，故把门槛钉死在能产生跨档样本的取值上。
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

/// 更长的补全**只是降级、没有被销毁** —— 它仍在候选里，翻页可及。
///
/// 这是仓库一贯的手法（同 step 6.5「降级不销毁」）：想要「根本不出现」应该去调
/// `[schema.pinyin.completion]` 的召回闸门，那是用户的旋钮，不是本档位的职责。
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
/// 这几条是 step 2c 与 step 6.5b 的产物，它们的音节数**本就与输入对齐**（`extra = 0`），
/// 故与本档位无冲突。缺了这条，「把所有多音节候选一律压到最后」这种过度实现也能让上面
/// 两条通过。
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
