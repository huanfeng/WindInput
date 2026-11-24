//! 临时英文复用英文方案词频（`schema.english.frequency`）与候选调整的端到端测试。
//!
//! 临英与英文方案共用 `"english"` 这一个数据桶，断言重心是读写两端确实落在同一个桶：
//!
//! - 写端：临英选词 → `store.get_freq("english", …)` 查得到；
//! - 读端：桶里有记录 → 临英候选顺序跟着变；
//! - 跨入口：临英学到的，切到英文方案照样生效（反过来同理）。
//!
//! 假绿防线：
//! 1. `schema.english.frequency.enabled` 出厂与 `Config::default()` 都是 `false`，须显式打开；
//! 2. 词典缺失时整族静默跳过（判据是耗时而非通过条数），worktree 里需自备 `build_dev`；
//! 3. 反向对照（`..._when_disabled`）排除「恒不记」的实现。

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

/// 英文方案就绪判据：方案定义与词库都要在（词库由构建 assemble 注入，不进 git）。
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
        seq_char: 0,
    }
}

/// 主方案刻意取五笔：临英的归属必须是内置英文方案，与 active 无关；active 若也是英文，
/// 「按 active 归属」的错误实现同样能通过。
fn temp_english_config(freq_enabled: bool, strategy: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    // 关掉大小写变形：它们每条占一个候选位，会把词库候选挤到下标不定的位置，
    // 断言「第 2 条是词库首条」就不再成立。变形本身另有测试覆盖。
    cfg.input.temp_english.case_variants = false;
    // 同理关掉大小写投影：这些断言拿候选面文本当词频键，投影会把它改写成所打形态
    // （`hello` → `Hello`），造出与被测通路无关的失配。投影下词频记原文另由 `english_case_follow.rs` 覆盖。
    cfg.input.temp_english.case_follow_input = false;
    cfg.schema.english.case_follow_input = false;
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

/// 临英当前页的词库候选（去掉首条原文）。
fn dict_texts(coord: &Coordinator) -> Vec<String> {
    coord.debug_page_texts().into_iter().skip(1).collect()
}

// ───────────────────────── 写端 ─────────────────────────

/// 临英选中词库候选 → 词频落进 `"english"` 桶（不是主方案 `wubi86`）。
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

/// 反向对照：调频开关关闭时一条都不记；同时钉住 `record_selection_in` 的开关按归属方案取
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

/// 原文候选（词库里没有的自造串）上屏 → 不记词频：它没有词库来源，记进去是读端永远查不中的
/// 孤儿键（同「短语恒不记词频」）。
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

/// `code_scope = "input"` 口径下，记账码必须是小写化的缓冲：临英缓冲带大写（Shift+H 进入即
/// `H`），英文方案那侧恒为全小写，不归一则两个入口静默地学不到一块去。
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

/// 桶里已有记录 → 临英候选跟着重排。直接往 store 种记录，使本条不依赖写端；策略取 `top`
/// 使断言确定（`position` 只前移一半，名次变化依赖初始下标）。
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

/// 取数上限须按词库方案（english）的引擎类型分级（300），不是写死 50：词频重排只能重排已在池中
/// 的候选。判据词 `technicians` 在 `t` 前缀里排第 150，换成前 50 的词就钉不住。断言用
/// `debug_all_candidate_texts` 而非只有当前页的 `dict_texts`。
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

/// 词频重排不得越过首候选：首条恒是用户所打原文，否则按空格会上屏一个没打的词。
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

/// 临英里选过的词，切到英文方案打同样的码照样受益；读写两端任一端取错方案时记账看着成功、
/// 候选顺序却永远不动。
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

    // 断言落在词库段首位（下标 1）而不是整列首位：英文方案的第 0 条恒是所打原文
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

/// 临英选词上屏补空格，开关是 `input.temp_english.commit_space`（临英自己那一份）：「长时打英文」
/// 与「中文里插一个英文词」的取舍可能相反，故与 `schema.english.commit_space` 分开。
#[test]
fn temp_english_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.input.temp_english.commit_space = true;
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
    cfg.input.temp_english.commit_space = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    let text = commit_text(&coord.handle_key_event(&key(VK_2, 0)));
    assert_eq!(text, picked, "开关关闭时不得补空格");
}

/// 两份开关互不越界：开英文方案那份，临英不得跟着补；排除「两个字段、内部仍读同一个」的实现。
#[test]
fn english_schema_switch_does_not_leak_into_temp_english() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.schema.english.commit_space = true; // 英文方案：开
    cfg.input.temp_english.commit_space = false; // 临英：关
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    let text = commit_text(&coord.handle_key_event(&key(VK_2, 0)));
    assert_eq!(
        text, picked,
        "临英该看自己那份开关：英文方案开着不代表临英也补空格"
    );
}

/// 回车上屏原文不补空格（终结性动作，与英文方案 `VK_RETURN` 空码分支同口径）。与
/// `temp_english_select_appends_space` 一起排除「恒补」与「恒不补」。
#[test]
fn temp_english_enter_never_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.input.temp_english.commit_space = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let text = commit_text(&coord.handle_key_event(&key(VK_RETURN, 0)));
    assert_eq!(text, "Hel", "回车上屏原文，且不补空格");
}

// ── 鼠标点选（`select_candidate_at` 的临英分支）────────────────────────────────
//
// 鼠标点选须走与键盘同一出口（补空格、记词频、全角转换）；通用 overlay 分支拿主路
// `input_buffer`（临英下恒空）判原码，三样都会漏。
// `debug_mouse_select` 取页内下标：0 = 原文，1 = 词库首条（本文件的配置关了变形）。

/// 鼠标点选词库候选上屏补空格，与数字键同口径。
#[test]
fn temp_english_mouse_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.input.temp_english.commit_space = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    let act = coord
        .debug_mouse_select(1)
        .expect("临英鼠标点选应带回上屏动作");
    assert_eq!(
        commit_text(&act),
        format!("{picked} "),
        "鼠标点选也应补一个空格"
    );
    assert_eq!(coord.debug_active_mode(), None, "上屏后退出临英");

    // 原文候选（无 source）同样补。
    enter_temp_english(&coord, "qzxv");
    let act = coord
        .debug_mouse_select(0)
        .expect("临英鼠标点选应带回上屏动作");
    assert_eq!(commit_text(&act), "Qzxv ", "点原文也应补一个空格");
}

/// 反向对照：开关关闭时鼠标点选不补。
#[test]
fn temp_english_mouse_select_no_space_when_disabled() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.input.temp_english.commit_space = false;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    let act = coord
        .debug_mouse_select(1)
        .expect("临英鼠标点选应带回上屏动作");
    assert_eq!(commit_text(&act), picked, "开关关闭时不得补空格");
}

/// 鼠标点选同样记临英词频（落 `english` 桶），与数字键同口径。
#[test]
fn temp_english_mouse_select_records_freq() {
    if !has_english_schema() {
        return;
    }
    let store = store_at("mouse_select");
    let coord = Coordinator::new_headless_with_store(
        temp_english_config(true, "position"),
        Some(&data_dir()),
        store.clone(),
    );

    enter_temp_english(&coord, "hel");
    let picked = dict_texts(&coord).first().cloned().expect("应有词库候选");
    coord
        .debug_mouse_select(1)
        .expect("临英鼠标点选应带回上屏动作");
    assert!(
        store
            .get_freq("english", &picked.to_lowercase(), &picked)
            .unwrap()
            .is_some(),
        "鼠标点选「{picked}」后应在 english 桶留下词频记录"
    );
}

/// 全角态下鼠标点选：正文转全角、补的是全角空格（U+3000），与键盘出口一致。
#[test]
fn temp_english_mouse_select_full_width() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = temp_english_config(false, "position");
    cfg.input.temp_english.commit_space = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    coord.handle_menu_command("toggle_width");

    enter_temp_english(&coord, "qzxv");
    let act = coord
        .debug_mouse_select(0)
        .expect("临英鼠标点选应带回上屏动作");
    assert_eq!(
        commit_text(&act),
        "Ｑｚｘｖ\u{3000}",
        "全角态应上屏全角正文 + 全角空格"
    );
}
