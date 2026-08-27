//! 临时英文复用英文方案词频（`schema.english.frequency`）与候选调整的端到端测试。
//!
//! # 这组测试要钉住的东西
//!
//! 临英此前**整条候选管线与主输入路是两套实现**：候选只经 `finalize_candidates`，上屏出口
//! 只接文本（`commit_temp_english_text(String)`）——候选的 `source` / `code` 在出口处就丢了，
//! 于是词频「不是漏调了一行」，而是根本没有可记的东西。改造后两个入口共用 `"english"` 这
//! 一个数据桶，本文件的断言重心就是**读写两端确实落在同一个桶**：
//!
//! - 写端：临英选词 → `store.get_freq("english", …)` 查得到；
//! - 读端：桶里有记录 → 临英候选顺序跟着变；
//! - 跨入口：临英学到的，切到英文方案照样生效（反过来同理）。
//!
//! # ⚠️ 两条最容易写出「假绿」的地方
//!
//! 1. **`schema.english.frequency.enabled` 出厂是 `false`，`Config::default()` 也是 `false`**。
//!    不显式打开，测的就是一个关着的功能——所有断言都会以「没记录」的方式"通过"某些写法。
//! 2. **词典缺失时整族静默跳过**（判据是耗时而非通过条数），worktree 里需自备 `build_dev`。
//!
//! 反向对照（`..._when_disabled`）不可省：没有它，「恒不记」与「按开关记」两种实现都能让
//! 正向断言通过——本仓 `english_commit_space` 的缺陷形态正是「代码看着对、通路走不到」。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;
const VK_2: u32 = 0x32;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 英文方案就绪判据：方案定义与**词库**都要在（词库由构建 assemble 注入，不进 git）。
fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
}

fn key(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 主方案刻意取**五笔**：临英的归属必须是内置英文方案，与 active 无关。active 若也是英文，
/// 「按 active 归属」这种错误实现同样能通过，等于什么都没锁住。
fn temp_english_config(freq_enabled: bool, strategy: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    // 关掉大小写变形：它们每条占一个候选位，会把词库候选挤到下标不定的位置，
    // 断言「第 2 条是词库首条」就不再成立。变形本身另有测试覆盖。
    cfg.input.temp_english.case_variants = false;
    cfg.schema.english.frequency.enabled = freq_enabled;
    cfg.schema.english.frequency.strategy = strategy.to_string();
    cfg
}

fn english_schema_config(freq_enabled: bool, strategy: &str) -> Config {
    let mut cfg = temp_english_config(freq_enabled, strategy);
    cfg.schema.active = "english".into();
    cfg
}

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_te_freq_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

/// Shift+首字母进入临英，再打完剩余字母（临英缓冲首字母恒大写，这正是取码要归一的原因）。
fn enter_temp_english(coord: &Coordinator, word: &str) {
    let mut chars = word.chars();
    let first = chars.next().expect("至少一个字母");
    let vk = (first.to_ascii_uppercase() as u32) & 0xFF;
    coord.handle_key_event(&key(vk, MOD_SHIFT));
    for c in chars {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// 主输入路打词（英文方案下，缓冲恒小写）。
fn type_word(coord: &Coordinator, word: &str) {
    for c in word.chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

fn commit_text(action: &KeyAction) -> String {
    match action {
        KeyAction::InsertText { text, .. } => text.clone(),
        other => panic!("应为 InsertText 上屏，实际: {other:?}"),
    }
}

/// 临英当前页的**词库候选**（去掉首条原文）。
fn dict_texts(coord: &Coordinator) -> Vec<String> {
    coord.debug_page_texts().into_iter().skip(1).collect()
}

// ───────────────────────── 写端 ─────────────────────────

/// 临英选中词库候选 → 词频落进 **`"english"` 桶**（不是主方案 `wubi86`）。
#[test]
fn temp_english_selection_records_into_english_bucket() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let store = store_at("write_bucket");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(true, "position"),
        Some(&data_dir()),
        store.clone(),
    );

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    coord.handle_key_event(&key(VK_2, 0)); // 数字键 2 = 第 2 条 = 词库首条

    // 默认 `code_scope = "candidate"`：记账码是候选自身的拼写（小写）。
    let code = picked.to_lowercase();
    assert!(
        store.get_freq("english", &code, &picked).unwrap().is_some(),
        "临英选中「{picked}」后应在 english 桶留下词频记录"
    );
    assert!(
        store.get_freq("wubi86", &code, &picked).unwrap().is_none(),
        "不得记进主方案的桶——归属按模式而非按 active"
    );
}

/// 反向对照：调频开关关闭时**一条都不记**。
///
/// 这条同时守住另一件事——`record_selection_in` 的开关是按**归属方案**取的
/// （`freq_settings_for("english")`），不是主方案的开关。
#[test]
fn temp_english_records_nothing_when_disabled() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("write_disabled");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(false, "position"),
        Some(&data_dir()),
        store.clone(),
    );

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    coord.handle_key_event(&key(VK_2, 0));

    assert!(
        store
            .get_freq("english", &picked.to_lowercase(), &picked)
            .unwrap()
            .is_none(),
        "开关关闭时不得记词频"
    );
}

/// 原文候选（词库里没有的自造串）上屏 → **不记词频**。
///
/// 它没有词库来源（`code` 空、`source` 为 `None`），记进去就是一条读端按候选码永远查不中的
/// 孤儿键，只会逐日累积垃圾。与「短语有文本无码位、恒不记词频」同一先例。
#[test]
fn temp_english_literal_text_is_not_recorded() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("write_literal");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(true, "position"),
        Some(&data_dir()),
        store.clone(),
    );

    enter_temp_english(&coord, "zzqx"); // 词库不会有的串
    let text = commit_text(&coord.handle_key_event(&key(VK_SPACE, 0)));
    assert_eq!(text.trim_end(), "Zzqx", "空格应上屏原文");

    assert!(
        store.get_freq("english", "zzqx", "Zzqx").unwrap().is_none(),
        "原文候选不得记词频（输入码口径）"
    );
    assert!(
        store.get_freq("english", "Zzqx", "Zzqx").unwrap().is_none(),
        "原文候选不得记词频（原样口径）"
    );
}

/// `code_scope = "input"` 口径下，记账码必须是**小写化的缓冲**。
///
/// 临英缓冲带大写（Shift+H 进入即 `H`），而英文方案那侧 `input_buffer` 恒为全小写。不归一
/// ⇒ `Hel` 与 `hel` 是两个键，两个入口永远学不到一块去，而这种失效完全静默。
#[test]
fn temp_english_input_scope_code_is_lowercased() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("write_case");
    let mut cfg = temp_english_config(true, "position");
    cfg.schema.english.frequency.code_scope = "input".into();
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    coord.handle_key_event(&key(VK_2, 0));

    assert!(
        store.get_freq("english", "hel", &picked).unwrap().is_some(),
        "input 口径的记账码应是小写化缓冲 hel"
    );
    assert!(
        store.get_freq("english", "Hel", &picked).unwrap().is_none(),
        "大写不得泄漏进词频键"
    );
}

// ───────────────────────── 读端 ─────────────────────────

/// 桶里已有记录 → 临英候选**跟着重排**（读端接上了）。
///
/// 直接往 store 里种记录而不是先选一遍：这样即便写端整个坏掉，本条仍能独立回答「读端通没通」。
/// 策略取 `top`（一次到顶 MRU）使断言确定——`position` 只前移一半，名次变化依赖初始下标。
#[test]
fn temp_english_candidates_follow_recorded_freq() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("read_rerank");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(true, "top"),
        Some(&data_dir()),
        store.clone(),
    );

    enter_temp_english(&coord, "hel");
    let before = dict_texts(&coord);
    if before.len() < 3 {
        eprintln!("跳过：hel 的词库候选不足 3 条");
        return;
    }
    let target = before[2].clone();

    // 种一条词频记录（默认 candidate 口径 ⇒ 键是候选自身拼写）。
    store
        .record_freq("english", &target.to_lowercase(), &target)
        .unwrap();

    // 重新进一次临英，触发候选重建。
    coord.handle_key_event(&key(0x1B, 0)); // Esc 退出
    enter_temp_english(&coord, "hel");
    let after = dict_texts(&coord);

    assert_eq!(
        after.first().map(String::as_str),
        Some(target.as_str()),
        "用过的词应升到词库段首位，实得 {after:?}"
    );
}

/// 取数上限须按**词库方案**（english）的引擎类型分级，不是写死 50。
///
/// # 判据词的位次就是这条测试的全部意义
///
/// `technicians` 在内置英文词库 `t` 前缀的 top-k 里排第 **150**：50 条取不到、300 条取得到。
/// 换一个排在前 50 的词，改前改后都能通过，等于什么都没锁住——本条已做变异验证（把上限
/// 改回 50 则精确变红）。
///
/// # 这条守的是什么缺陷
///
/// 真机现象：英文方案下打 `t` 能出刚用过的词，临英下同一个 `t` 出不来，打到 `th` 又正常。
/// 根因不在词频链路（读写两端都正常），而在**候选池**——词频重排只能重排已在池中的候选，
/// 取不到就无从谈起。临英此前写死 `ENGINE_MAX_CANDIDATES`(50)，而主输入路按引擎类型取 300。
///
/// ⚠️ 断言用 `debug_all_candidate_texts` 而非 `dict_texts`：后者只有**当前页**，一页装不下
/// 第 150 名，用它会让「进没进池」与「排到第几」两件事混在一起。
#[test]
fn temp_english_limit_follows_dict_schema_engine_type() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    const TARGET: &str = "technicians";
    let store = store_at("limit_scope");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(true, "top"),
        Some(&data_dir()),
        store.clone(),
    );
    // 直接种记录：默认 `code_scope = "candidate"`，记账码即候选自身拼写。
    store.record_freq("english", TARGET, TARGET).unwrap();

    enter_temp_english(&coord, "t");

    let all = coord.debug_all_candidate_texts();
    assert!(
        all.iter().any(|t| t == TARGET),
        "「{TARGET}」在 t 的 top-k 里排第 150，上限 300 时必须进池（实得 {} 条候选）",
        all.len()
    );
    assert_eq!(
        dict_texts(&coord).first().map(String::as_str),
        Some(TARGET),
        "唯一有词频记录者，`top` 策略下应升到词库段首位"
    );
}

/// 词频重排**不得越过首候选**：首条恒是用户所打原文。
///
/// 这是临英的硬承诺——打词库里没有的词时，原文是唯一能上屏的东西。重排一旦作用于整个列表，
/// 用户按空格就会上屏一个他没打的词。
#[test]
fn temp_english_rerank_never_displaces_literal_first() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("read_first");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(true, "top"),
        Some(&data_dir()),
        store.clone(),
    );

    enter_temp_english(&coord, "hel");
    let dict = dict_texts(&coord);
    if dict.len() < 3 {
        return;
    }
    store
        .record_freq("english", &dict[2].to_lowercase(), &dict[2])
        .unwrap();

    coord.handle_key_event(&key(0x1B, 0));
    enter_temp_english(&coord, "hel");

    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("Hel"),
        "首候选必须仍是输入原文"
    );
}

// ───────────────────────── 跨入口共享 ─────────────────────────

/// **本组的核心**：临英里选过的词，切到英文方案打同样的码照样受益。
///
/// 读写两端只要有一端取错方案（比如写进 `wubi86`、读的是 `english`），记账看着成功、候选
/// 顺序永远不动——这正是本仓反复栽过的那种完全静默的失配。
#[test]
fn temp_english_freq_carries_over_to_english_schema() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("shared_bucket");

    // ① 临英里选中第 3 条词库候选。
    let picked = {
        let coord = Coordinator::new_headless_with_store(
            temp_english_config(true, "top"),
            Some(&data_dir()),
            store.clone(),
        );
        enter_temp_english(&coord, "hel");
        let dict = dict_texts(&coord);
        if dict.len() < 3 {
            eprintln!("跳过：hel 的词库候选不足 3 条");
            return;
        }
        let target = dict[2].clone();
        // 数字键 4 = 页内第 4 条 = 原文 + 词库前三条里的第三条。
        coord.handle_key_event(&key(0x34, 0));
        target
    };

    // ② 换一个 active=english 的 Coordinator（同一个 store）打同样的码。
    let coord = Coordinator::new_headless_with_store(
        english_schema_config(true, "top"),
        Some(&data_dir()),
        store.clone(),
    );
    assert_eq!(coord.active_schema_id(), "english");
    type_word(&coord, "hel");

    // ⚠️ 断言落在**词库段首位**（下标 1）而不是整列首位：英文方案的第 0 条恒是所打原文
    // （`schema.english.raw_candidate` 默认开），调频只在词库段内部生效——原文与变形被
    // `split_off(dict_start)` 挡在重排之外，见 `docs/design/schema-scoped-behavior.md` §5.2。
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("hel"),
        "英文方案首候选恒是所打原文，实得 {page:?}"
    );
    assert_eq!(
        page.get(1).map(String::as_str),
        Some(picked.as_str()),
        "临英里选过的「{picked}」应在英文方案下升到词库段首位，实得 {page:?}"
    );
}

// ───────────────────────── 补空格 ─────────────────────────

/// 临英选词上屏同样补空格（`schema.english.commit_space`）——与英文方案同一个开关。
#[test]
fn temp_english_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.schema.english.commit_space = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    let text = commit_text(&coord.handle_key_event(&key(VK_2, 0)));
    assert_eq!(text, format!("{picked} "), "选词上屏应补一个空格");
}

/// 反向对照：开关关闭时不补。
#[test]
fn temp_english_no_space_when_disabled() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.schema.english.commit_space = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    let text = commit_text(&coord.handle_key_event(&key(VK_2, 0)));
    assert_eq!(text, picked, "开关关闭时不得补空格");
}

/// 回车上屏原文**不补空格**——终结性动作，与英文方案 `VK_RETURN` 空码分支同口径。
///
/// ⚠️ 这条与上面 `temp_english_select_appends_space` 是同一开关下的**相反**期望。两条都在，
/// 才能把「恒补」和「恒不补」两种实现同时排除。
#[test]
fn temp_english_enter_never_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.schema.english.commit_space = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let text = commit_text(&coord.handle_key_event(&key(VK_RETURN, 0)));
    assert_eq!(text, "Hel", "回车上屏原文，且不补空格");
}
