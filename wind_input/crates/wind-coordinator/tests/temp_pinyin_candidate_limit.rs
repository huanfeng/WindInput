//! 临时拼音的候选取数上限与检索范围过滤回归测试
//!
//! **取数上限**：临拼曾固定向引擎取 `ENGINE_MAX_CANDIDATES`(50) 条，而它**没有翻页扩容
//! 通路**（`expand_candidates` 的守卫比对 `input_buffer`，临拼的码在 `temp_pinyin_buffer`
//! 里），于是第 51 位之后的候选**翻多少页都取不到**。用户实测：临拼下 `ying` 打不出「瑩」
//! （该字在拼音候选的第 158 位）。修复是让拼音类目标方案取全量
//! （`TEMP_PINYIN_MAX_CANDIDATES`），翻页由对 `state.candidates` 切片天然穷尽。
//!
//! **检索范围过滤**：临拼此前完全不经过 `apply_filter`——「检索范围」设置对它从来无效，
//! 默认 smart 下临拼比主路径多出数百个生僻字（实测 `ying`：299 vs 76）。现已按主路径同序
//! 接入（`mark_common` → `apply_filter`）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 词库缺失时跳过（无数据 CI 环境）。⚠️ 判据是耗时：本测试族真跑时约 0.15s 以上，
/// 若整体 0.00s 说明走了跳过分支，不能当作通过。
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
/// 断言刻意包含**位置 > 50**：这是「测试没有恰好通过」的证据——若取数上限被改回 50
/// 之类的小值，该字不再在场，测试即红。只断言「包含瑩」是不够的，那在别的实现下
/// 也可能因排序变化而偶然满足。
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

/// 临拼一次取到的候选量，不得少于主路径首屏 —— 即「翻页有内容可翻」。
///
/// ⚠️ **不能断言两者相等**。前缀补全的条数现在跟随请求量
/// （`pinyin/mod.rs` 的 `completion_limit`），而临拼一次取 `TEMP_PINYIN_MAX_CANDIDATES`、
/// 主路径首屏只取 `initial_candidate_limit`(300) 并靠翻页逐步扩容 —— 两者本就不同。
/// 早期版本曾断言相等，那建立在「补全固定 30 条、两侧都取到同一个全量」的旧假设上，
/// 放开补全后该前提即失效。
///
/// 真正要钉的是：临拼**不比主路径少**，且拿到了该输入的全部精确匹配。
///
/// ⚠️ **数字随 step 6.3 音节数闸门变过一次**（`STRICT_SYLLABLE_MATCH_MAX`）。`ying` 是
/// 单音节输入，其前缀补全（「影响」「英语」…）现已整体不产出，总数从 1248 降到 293：
/// 272 条 `code == ying` 的精确同音字（**改动前后一条不差**）+ 21 条切分为 `yin|g…` 的词
/// （「因果」「因故」，第 2 个音节起点落在输入内 ⇒ used=2 ⇒ 不进严格档，属合法候选）。
/// 故这里改用精确同音字量级作判据；「取数上限没被压低」另有
/// [`temp_pinyin_reaches_rare_char_beyond_old_limit`]（「瑩」须在第 50 位之外）语义化守卫。
///
/// 两侧都用 `gb18030`（不过滤）以统一口径；过滤本身由
/// [`temp_pinyin_respects_filter_mode`] 单独覆盖。
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
/// **自带反向对照**：同一输入在 `smart` 下生僻字「瑩」应被滤掉、在 `gb18030` 下应在场。
/// 只测其中一侧都不足以证明过滤真的接上了——只测 smart 无法区分「过滤生效」与「取数
/// 上限又退回 50」，只测 gb18030 则根本不经过过滤分支。
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

/// ★ 临拼也支持末页翻页放宽，与主路径同一套机制。
///
/// 两处判据必须按模式分流，否则临拼下**静默失效**：
/// - 触发（`try_relax_scope_on_page_end`）：临拼的码在 `temp_pinyin_buffer`，用
///   `input_buffer` 一刀切会让它永远触发不了（那边恒为空）；
/// - 失效（`expire_scope_override`）：同样的原因，会让放宽在下一次按键就被清掉。
///
/// 反向对照用 `gb18030`：那里本就不过滤，放宽前后候选数必须相等——证明多出来的候选确实
/// 来自「被智能档滤掉的那些」，而不是翻页顺带触发了别的扩容通路。
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

/// ★ 临拼的候选顺序必须与**直接用该拼音方案打字**一致。
///
/// 回归（用户报障）：临拼下打 `ni` 首选是「年」，整页被「你的」「你们」等高频词组占满，
/// 而全拼下首选是「你」。两个成因叠加：
///
/// 1. 临拼用**纯 weight 排序**（`b.weight.cmp(&a.weight).then(natural_order)`），缺了
///    `candidate_display_order` 的 `is_prefix` 层 ⇒ 前缀补全（「年」nian 是 ni 的扩展）
///    压过精确匹配；
/// 2. 就算换成 `candidate_display_order`，`ignore_weight` 若取
///    `active_base_sort_ignores_weight()` 拿到的是**活跃方案（五笔）**的 `base_sort`——
///    用码表的排序配置排拼音候选，即用户直觉说的「被五笔干扰」。须按临拼**目标方案**取
///    （`base_sort_ignores_weight_of`）。
///
/// 断言整体列表而非只比首选：只比首选时，一个「把词组整体沉底」的错误实现也能碰巧通过。
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

/// ★★ **四条拼音路径的候选顺序必须一致**：纯拼音方案 / 临时拼音 / 快捷输入 member 拼音，
/// 三者逐项相等；混输的拼音部分保持相同相对顺序。
///
/// 这四条路径的候选装配方式各不相同，历史上正因如此走散过：
///
/// | 路径 | 装配 | 排序 |
/// |---|---|---|
/// | 纯拼音 | `build_candidates` | 合并短语后 `candidate_display_order` 重排 |
/// | 临时拼音 | `update_temp_pinyin_candidates` | 同上（**曾用纯 weight，是本测试要防的回归**）|
/// | 快捷输入 | `handle_mode` 按 member 汇总 | **不重排**，保持引擎顺序 + member 优先级 |
/// | 混输 | `MixedEngine` 内部融合 | 引擎内按档位（码表精确 > 拼音）|
///
/// 三者一致同时证明了一件事：**拼音引擎自身给出的顺序就等于 `candidate_display_order`**
/// （快捷输入不重排却与重排过的纯拼音相同）。所以临拼当初的 bug 本质是「在已正确的引擎
/// 顺序上又用错误规则重排了一次」——纯 weight 排序缺 `is_prefix` 层，前缀补全「年」(nian)
/// 与高频词组「你的」压过了精确匹配的单字「你」。
///
/// 混输单独比对：它首位是**五笔码表候选**（`ni` → 二简「悄」w9950，正是 gen_dict 给二简的
/// 钦定权重），码表精确匹配优先于拼音属设计（见 mixed_pinyin_exact_tier），故只断言其中
/// 拼音候选的**相对顺序**不变。
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

        // 混输：滤出两边共有的候选，比对**前 20 位**的相对顺序（码表候选允许插在其间）。
        //
        // 只比前 20 而非全表：实测两侧在第 100 位附近会有个别同权重候选互换（如「念」与
        // 「你看看」），那是稳定排序下**输入顺序不同**导致的——混输的拼音候选经过与码表的
        // 融合，进入排序时的原始次序本就与纯拼音不同。深尾部的这种抖动不影响使用，
        // 断言全表只会让测试变脆。
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
