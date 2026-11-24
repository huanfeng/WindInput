//! 临时拼音的候选取数上限与检索范围过滤回归测试
//!
//! 取数上限：临拼没有翻页扩容通路（`expand_candidates` 的守卫比对 `input_buffer`，临拼的码在
//! `temp_pinyin_buffer` 里），因此拼音类目标方案取全量（`TEMP_PINYIN_MAX_CANDIDATES`），
//! 翻页由对 `state.candidates` 切片穷尽；取少了深位候选（如 `ying` 的「瑩」）翻多少页都到不了。
//!
//! 检索范围过滤：临拼按主路径同序接入 `mark_common` → `apply_filter`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 词库缺失时跳过（无数据 CI 环境）。本测试族真跑约 0.15s 以上，整体 0.00s 说明走了跳过分支。
fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/pinyin.schema.toml").exists()
        && d.join("schemas/pinyin/cn_dicts/41448.dict.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
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

fn press_letter(coord: &Coordinator, c: char) -> KeyAction {
    coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF))
}

/// 五笔方案（临拼只在码表/混输方案下可用）+ 指定检索范围。
fn config(filter_mode: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = filter_mode.into();
    cfg
}

/// 进入临拼并输入给定拼音，返回全部候选文本。
fn temp_pinyin_candidates(filter_mode: &str, input: &str) -> Vec<String> {
    let coord = Coordinator::new_headless(config(filter_mode), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xC0)); // 反引号进入临拼
    assert!(coord.debug_in_temp_pinyin(), "反引号应进入临时拼音");
    for c in input.chars() {
        press_letter(&coord, c);
    }
    coord.debug_all_candidate_texts()
}

/// 生僻字必须可达：检索范围为「全部字符」时 `ying` → 「瑩」。
///
/// 断言刻意包含位置 > 50：取数上限被压回小值时该字不在场即红；只断言「包含」可能因排序变化偶然满足。
#[test]
fn temp_pinyin_reaches_rare_char_beyond_old_limit() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let all = temp_pinyin_candidates("gb18030", "ying");
    let pos = all.iter().position(|t| t == "瑩");
    assert!(
        pos.is_some(),
        "临拼 `ying` 应能取到生僻字「瑩」，实际候选数={}",
        all.len()
    );
    let pos = pos.unwrap();
    assert!(
        pos > 50,
        "「瑩」应落在旧上限(50)之外（实测约第 158 位），否则本测试证明不了扩容生效：实际位置={pos}"
    );
}

/// 临拼一次取到的候选量不得少于主路径首屏，且拿到了该输入的全部精确匹配——即「翻页有内容可翻」。
///
/// 不能断言两者相等：前缀补全条数跟随请求量（`pinyin/mod.rs` 的 `completion_limit`），临拼取
/// `TEMP_PINYIN_MAX_CANDIDATES`，主路径首屏只取 `initial_candidate_limit` 并靠翻页扩容。
/// 单音节 `ying` 受 step 6.3 音节数闸门（`STRICT_SYLLABLE_MATCH_MAX`）影响不产出前缀补全，
/// 故以精确同音字量级作判据；「取数上限没被压低」由
/// [`temp_pinyin_reaches_rare_char_beyond_old_limit`] 守。
///
/// 两侧都用 `gb18030`（不过滤）统一口径；过滤由 [`temp_pinyin_respects_filter_mode`] 覆盖。
#[test]
fn temp_pinyin_candidate_count_not_less_than_main_path() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let temp = temp_pinyin_candidates("gb18030", "ying");

    // 主路径对照：纯拼音方案下输入同样的码。
    let mut cfg = config("gb18030");
    cfg.schema.active = "pinyin".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in "ying".chars() {
        press_letter(&coord, c);
    }
    let main = coord.debug_all_candidate_texts();

    assert!(
        main.len() > 50,
        "对照组自身须超过旧上限，否则比较无意义：main={}",
        main.len()
    );
    assert!(
        temp.len() >= main.len(),
        "临拼取数不应少于主路径首屏（临拼={} 主路径={}）",
        temp.len(),
        main.len()
    );
    // 精确匹配必须完整：`ying` 有 272 条同音字，取不全即说明取数上限又被压低。
    assert!(
        temp.len() > 250,
        "临拼应取到 `ying` 的全部同音字（272 条），实际只有 {}",
        temp.len()
    );
}

/// 临拼必须遵守「检索范围」设置。
///
/// 自带反向对照：`smart` 下「瑩」应被滤掉、`gb18030` 下应在场。只测 smart 分不清「过滤生效」与
/// 「取数上限退回 50」，只测 gb18030 则根本不经过过滤分支。
#[test]
fn temp_pinyin_respects_filter_mode() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let all = temp_pinyin_candidates("gb18030", "ying");
    let smart = temp_pinyin_candidates("smart", "ying");

    assert!(
        all.iter().any(|t| t == "瑩"),
        "全部字符下「瑩」应在场（对照组）"
    );
    assert!(
        !smart.iter().any(|t| t == "瑩"),
        "智能过滤下生僻字「瑩」应被滤掉——临拼未接 apply_filter 时此断言必红"
    );
    assert!(
        smart.len() < all.len(),
        "智能过滤应使候选变少（smart={} 全部={}）",
        smart.len(),
        all.len()
    );
    // 常用字不受影响。
    assert!(
        smart.iter().any(|t| t == "应"),
        "常用字「应」在智能过滤下仍应在场"
    );
}

/// 翻页不改变候选集合：临拼靠对 `state.candidates` 切片翻页，不重新查询，
/// 故翻到底也不会新增或丢失候选。
#[test]
fn temp_pinyin_paging_does_not_change_candidate_set() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(config("gb18030"), Some(&data_dir()));
    coord.handle_key_event(&key_event(0xC0));
    for c in "ying".chars() {
        press_letter(&coord, c);
    }
    let before = coord.debug_all_candidate_texts();
    for _ in 0..60 {
        coord.handle_key_event(&key_event(0x22)); // PageDown
    }
    let after = coord.debug_all_candidate_texts();
    assert_eq!(before, after, "翻页不应改变候选集合");
    assert!(after.iter().any(|t| t == "瑩"), "翻页后「瑩」仍应在候选中");
}

/// 临拼也支持末页翻页放宽，与主路径同一套机制。
///
/// 两处判据必须按模式分流（临拼的码在 `temp_pinyin_buffer`，`input_buffer` 恒为空），否则静默失效：
/// - 触发（`try_relax_scope_on_page_end`）：永远触发不了；
/// - 失效（`expire_scope_override`）：放宽在下一次按键就被清掉。
///
/// 反向对照用 `gb18030`：本就不过滤，放宽前后候选数必须相等，证明多出的候选来自被智能档滤掉的那些。
#[test]
fn temp_pinyin_relaxes_scope_on_page_end() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let coord = Coordinator::new_headless(config("smart"), Some(&data_dir()));
    coord.handle_key_event_policed(&key_event(0xC0)); // 进入临拼
    for c in "ying".chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    let before = coord.debug_all_candidate_texts();

    // 翻到末页，再按一次触发放宽
    for _ in 0..200 {
        let (cur, _, total) = coord.debug_page_info();
        if cur + 1 >= total {
            break;
        }
        coord.handle_key_event_policed(&key_event(0x22)); // PageDown
    }
    coord.handle_key_event_policed(&key_event(0x22));

    let after = coord.debug_all_candidate_texts();
    assert!(
        after.len() > before.len(),
        "临拼末页再翻页应放出被滤的生僻字：{} → {}",
        before.len(),
        after.len()
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "放宽不得改动原有候选的顺序（追加在末尾）"
    );
    // 放宽后应能取到智能档下被滤掉的生僻字
    assert!(
        after.contains(&"瑩".to_string()),
        "放宽后 `ying` 应能打出「瑩」"
    );
}

/// 直接用全拼方案（非临拼）打字，作为临拼的对照基准。
fn plain_pinyin_candidates(filter_mode: &str, input: &str) -> Vec<String> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = filter_mode.into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in input.chars() {
        press_letter(&coord, c);
    }
    coord.debug_all_candidate_texts()
}

/// 临拼的候选顺序必须与直接用该拼音方案打字一致（`ni` 首选是「你」）。
///
/// 两个要点：
/// 1. 须用 `candidate_display_order`，纯 weight 排序缺 `is_prefix` 层，前缀补全（「年」nian）
///    会压过精确匹配；
/// 2. `ignore_weight` 须按临拼目标方案取（`base_sort_ignores_weight_of`），
///    不能用 `active_base_sort_ignores_weight()`——那是活跃码表方案的 `base_sort`。
///
/// 断言整体列表：只比首选时「把词组整体沉底」的错误实现也能碰巧通过。
#[test]
fn temp_pinyin_order_matches_plain_pinyin() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    for input in ["ni", "shi", "zhongguo", "xian"] {
        let temp = temp_pinyin_candidates("smart", input);
        let plain = plain_pinyin_candidates("smart", input);
        assert!(
            !temp.is_empty() && !plain.is_empty(),
            "前提：{input} 两条路径都应有候选"
        );
        let n = 10.min(temp.len()).min(plain.len());
        assert_eq!(
            &temp[..n],
            &plain[..n],
            "临拼 `{input}` 的候选应与全拼一致。\n临拼: {:?}\n全拼: {:?}",
            &temp[..n],
            &plain[..n]
        );
    }
}

/// 混输方案（码表 + 拼音）下打字，返回候选。
fn mixed_candidates(input: &str) -> Vec<String> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    for c in input.chars() {
        press_letter(&coord, c);
    }
    coord.debug_all_candidate_texts()
}

/// 快捷输入（`;` 进入 quick_mix，其 members 含 `$primary_pinyin`）下打字，返回候选。
fn quick_input_candidates(input: &str) -> Vec<String> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_key_event_policed(&key_event(0xBA)); // ';' 触发 quick_mix
    for c in input.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    coord.debug_all_candidate_texts()
}

/// 四条拼音路径的候选顺序必须一致：纯拼音方案 / 临时拼音 / 快捷输入 member 拼音三者逐项相等；
/// 混输的拼音部分保持相同相对顺序。
///
/// | 路径 | 装配 | 排序 |
/// |---|---|---|
/// | 纯拼音 | `build_candidates` | 合并短语后 `candidate_display_order` 重排 |
/// | 临时拼音 | `update_temp_pinyin_candidates` | 同上 |
/// | 快捷输入 | `handle_mode` 按 member 汇总 | 不重排，保持引擎顺序 + member 优先级 |
/// | 混输 | `MixedEngine` 内部融合 | 引擎内按档位（码表精确 > 拼音）|
///
/// 快捷输入不重排却与纯拼音相同，说明拼音引擎自身的顺序就等于 `candidate_display_order`，
/// 下游重排只能用同一规则。混输首位是五笔码表候选（码表精确优先于拼音属设计，见
/// mixed_pinyin_exact_tier），故只断言其中拼音候选的相对顺序。
#[test]
fn all_pinyin_paths_share_same_order() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    for input in ["ni", "shi"] {
        let plain = plain_pinyin_candidates("smart", input);
        let temp = temp_pinyin_candidates("smart", input);
        let quick = quick_input_candidates(input);
        assert!(plain.len() >= 8, "前提：{input} 应有足够候选");

        let n = 8.min(plain.len()).min(temp.len()).min(quick.len());
        assert_eq!(
            &temp[..n],
            &plain[..n],
            "临拼 `{input}` 应与纯拼音一致\n临拼: {:?}\n纯拼音: {:?}",
            &temp[..n],
            &plain[..n]
        );
        assert_eq!(
            &quick[..n],
            &plain[..n],
            "快捷输入 `{input}` 的拼音候选应与纯拼音一致\n快捷: {:?}\n纯拼音: {:?}",
            &quick[..n],
            &plain[..n]
        );

        // 混输：滤出两边共有的候选，比对前 20 位的相对顺序（码表候选允许插在其间）。
        // 深尾部会有个别同权重候选因融合后输入次序不同而互换，断言全表只会让测试变脆。
        let take = 20;
        let mixed = mixed_candidates(input);
        let mixed_seq: Vec<&String> = mixed
            .iter()
            .filter(|t| plain.contains(t))
            .take(take)
            .collect();
        let plain_seq: Vec<&String> = plain
            .iter()
            .filter(|t| mixed.contains(t))
            .take(take)
            .collect();
        assert_eq!(
            mixed_seq, plain_seq,
            "混输 `{input}` 中拼音候选（前 {take} 位）的相对顺序应与纯拼音一致"
        );
    }
}
