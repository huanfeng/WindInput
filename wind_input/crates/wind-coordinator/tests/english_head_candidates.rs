//! 英文方案与临时英文的**头部候选**（输入原文 + 大小写变形）端到端测试。
//!
//! 设计见 `docs/design/schema-scoped-behavior.md` §5。
//!
//! # 这组测试要钉住的三件事
//!
//! 1. **英文方案下首候选恒是所打原文**——这是本功能的全部意义。英文引擎的「输入即内容」
//!    使输入串本身就是可上屏文本，而调频会把某个词顶到首位，届时想上屏原文就只剩回车。
//! 2. **四个键两侧独立**：`schema.english.*` 与 `input.temp_english.*` 互不影响，
//!    且 `case_variants` 两侧**默认值相反**（英文方案 false / 临英 true）。
//! 3. ★ **两个开关同时关 + 词库无命中 ⇒ 候选为空**时，空格仍必须上屏输入串。
//!    这是设计文档 §5.5 标为「实施时必验」的一条——英文方案侧走主路径的通用分支，
//!    是整个设计里唯一没有既存判据可依的地方。若它吞键，表现就是「打了一串英文按空格
//!    什么都没发生」。
//!
//! # ⚠️ 假绿源
//!
//! 词典缺失时整族**静默跳过**（判据是耗时而非通过条数），worktree 需自备 `build_dev`。
//! 见 `has_english_schema`。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

const VK_SPACE: u32 = 0x20;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

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

/// 英文方案作 active。
fn english_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 临英（主方案取**五笔**：归属必须是内置英文方案，与 active 无关）。
fn temp_english_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    cfg
}

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_en_head_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

fn coord_with(cfg: Config, tag: &str) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store_at(tag))
}

/// 主输入路打词（英文方案下缓冲恒小写）。
fn type_word(coord: &Coordinator, word: &str) {
    for c in word.chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// Shift+首字母进入临英，再打完剩余字母。
fn enter_temp_english(coord: &Coordinator, word: &str) {
    let mut chars = word.chars();
    let first = chars.next().expect("至少一个字母");
    coord.handle_key_event(&key((first.to_ascii_uppercase() as u32) & 0xFF, MOD_SHIFT));
    for c in chars {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

// ───────────────────── 英文方案：原文候选 ─────────────────────

/// 英文方案下首候选恒是所打原文，其后才是词库补全。
#[test]
fn english_schema_first_candidate_is_the_raw_input() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = coord_with(english_config(), "raw_on");
    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("hel"),
        "首候选必须是所打原文，实际页面: {page:?}"
    );
    assert!(
        page.len() > 1,
        "原文之后应还有词库补全（hello/help/…），实际: {page:?}"
    );
}

/// 关掉 `raw_candidate`：首候选变成词库词，原文不再单独占位。
///
/// 反向对照不可省：没有它，「恒插原文」与「按开关插原文」两种实现都能让正向断言通过。
#[test]
fn english_schema_raw_candidate_can_be_turned_off() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let coord = coord_with(cfg, "raw_off");
    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert!(!page.is_empty(), "词库应有 hel 的前缀命中");
    assert_ne!(
        page.first().map(String::as_str),
        Some("hel"),
        "关掉后首条应是词库词而非原文，实际: {page:?}"
    );
}

/// 词频把某个词顶到词库段首时，原文**仍在它之前**。
///
/// 这条才是需求的原始场景：调频本身工作正常，但用户还要能一键上屏所打原文。
/// 断言落在「原文在前、被顶起来的词紧随其后」，两件事同时成立才算对。
#[test]
fn raw_candidate_outranks_frequency_promoted_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    // ⚠️ 出厂是 false，不显式打开就是在测一个关着的功能。
    cfg.schema.english.frequency.enabled = true;
    cfg.schema.english.frequency.strategy = "top".into();
    let coord = coord_with(cfg, "freq_top");

    // 先选一次靠后的词，把它顶到词库段首。
    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    // page[0] 是原文，词库段从 1 开始；取第 2 个词库候选（越靠后越能证明"被顶起来了"）。
    let target = page.get(2).cloned().unwrap_or_default();
    if target.is_empty() {
        eprintln!("跳过：hel 的词库候选不足 2 条");
        return;
    }
    // 数字键 3 选中它（1 是原文）。
    coord.handle_key_event(&key(0x33, 0));

    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("hel"),
        "调频把词顶到词库段首后，原文仍必须在最前，实际: {page:?}"
    );
    assert_eq!(
        page.get(1),
        Some(&target),
        "被调频顶起来的词应紧随原文之后（证明调频确实生效了，不是这条测试自己没跑起来）"
    );
}

// ───────────────────── 大小写变形：两侧默认值相反 ─────────────────────

/// 英文方案默认**不**出变形候选；临英默认**出**。
///
/// 默认值本身要有测试，否则「只翻默认值」一条测试都不会红。
#[test]
fn case_variants_defaults_differ_between_the_two_scopes() {
    let cfg = Config::default();
    assert!(
        !cfg.schema.english.case_variants,
        "英文方案是长时输入场景，变形每条吃一个候选位，默认应关"
    );
    assert!(
        cfg.input.temp_english.case_variants,
        "临英是「中文里插一个英文词」，首字母大写是刚需，默认应开（既有行为）"
    );
    // 升级成三档后默认仍是 `Always`（＝老配置的 `true`）：改默认值就是给所有存量用户
    // 换行为，而他们一个设置都没动过。
    assert_eq!(
        (
            cfg.schema.english.raw_candidate,
            cfg.input.temp_english.raw_candidate
        ),
        (RawCandidateMode::Always, RawCandidateMode::Always),
        "原文候选两侧都默认恒出：临英是保持既有行为，英文方案是需求的核心诉求"
    );
}

/// 两侧的键互不影响：改英文方案那对，临英的产出一个字都不变。
#[test]
fn the_two_scopes_do_not_leak_into_each_other() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    // 英文方案侧全关，临英侧保持默认（原文 + 变形）。
    let mut cfg = temp_english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    cfg.schema.english.case_variants = false;
    let coord = coord_with(cfg, "no_leak");
    enter_temp_english(&coord, "Hel");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("Hel"),
        "临英首候选仍是原文——改英文方案那对键不该影响临英，实际: {page:?}"
    );
    assert!(
        page.iter().any(|t| t == "hel") && page.iter().any(|t| t == "HEL"),
        "临英仍应出大小写变形，实际: {page:?}"
    );
}

// ───────────────────── ★ §5.5：候选为空时的上屏出口 ─────────────────────

/// ★ 英文方案：两个开关都关 + 词库无命中 ⇒ 候选为空，**空格必须上屏输入串**。
///
/// 设计文档标为「实施时必验」的一条。英文方案走主路径的通用分支，没有既存判据可依；
/// 若它吞键，表现就是「打了一串英文按空格什么都没发生」——用户会以为输入法死了。
#[test]
fn english_schema_commits_raw_input_when_no_candidates() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    cfg.schema.english.case_variants = false;
    let coord = coord_with(cfg, "empty_commit");
    // 刻意打一个词库里不可能有的串。
    type_word(&coord, "zzqxwv");
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提不成立：这串码不该有词库候选，实际: {:?}",
        coord.debug_page_texts()
    );
    let action = coord.handle_key_event(&key(VK_SPACE, 0));
    match action {
        KeyAction::InsertText { text, .. } => assert!(
            text.starts_with("zzqxwv"),
            "空候选时空格应上屏输入串本身，实际上屏: {text:?}"
        ),
        other => panic!(
            "空候选时空格必须上屏输入串，不得吞键。实际: {other:?}\n\
             （若这里是 Eaten/None，表现就是「打了一串英文按空格什么都没发生」）"
        ),
    }
}

/// ★ 临英：同样的组合下空格上屏缓冲原文。
///
/// 临英侧**判据本来就是对的**——空格臂判的是 `!candidates.is_empty()`（实际候选）
/// 而不是 `show_candidates` 配置项，所以空候选会正确落到「上屏缓冲原文」的兜底分支。
/// 这条测试是为了把「本来就对」变成「被钉住了」：那个分支的注释写的是
/// 「无候选（show_candidates 关闭）」，成因如今多了一个。
#[test]
fn temp_english_commits_buffer_when_no_candidates() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = temp_english_config();
    cfg.input.temp_english.raw_candidate = RawCandidateMode::Off;
    cfg.input.temp_english.case_variants = false;
    let coord = coord_with(cfg, "te_empty_commit");
    enter_temp_english(&coord, "Zzqxwv");
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提不成立：这串不该有词库候选，实际: {:?}",
        coord.debug_page_texts()
    );
    let action = coord.handle_key_event(&key(VK_SPACE, 0));
    match action {
        KeyAction::InsertText { text, .. } => assert!(
            text.starts_with("Zzqxwv"),
            "空候选时空格应上屏缓冲原文，实际上屏: {text:?}"
        ),
        other => panic!("空候选时空格必须上屏缓冲原文，不得吞键。实际: {other:?}"),
    }
}

// ─────────────── `RawCandidateMode::InDict`：原文只在它是词库词时才当首选 ───────────────
//
// t139 / A2-2。现有两档各对一半：`Always` 下打 `hel`（词库无此词）会多出一条占着首位的
// `hel`；`Off` 下打 `hell`（词库有）又会被调频顶下去（`hello` 用得多就排到了前面）。
// `InDict` 是那条缺失的对角线——按「原文本身是不是词库词」在另外两档之间逐次切换。
//
// ⚠️ 下面每条正向用例都配了**反向对照**：只测 `InDict` 的话，把它实现成 `Always` 或
// `Off` 的别名同样能让一半用例通过。

/// 词库里**没有**这个词 ⇒ 一条原文候选都不产，列表是纯词库候选、调频照常生效。
#[test]
fn in_dict_drops_raw_when_input_is_not_a_dict_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::InDict;
    let coord = coord_with(cfg, "indict_miss");
    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert!(
        !page.contains(&"hel".to_string()),
        "`hel` 不是词库词，InDict 档下不该产出这条原文候选（实际：{page:?}）"
    );
    assert!(
        page.first().is_some_and(|c| c.len() > 3),
        "首选应是词库补全（hello / hell / …），实际：{page:?}"
    );
}

/// 词库里**有**这个词 ⇒ 钉首位，且**压过调频**（对照组里 `hello` 已被用了 9 次）。
#[test]
fn in_dict_pins_raw_above_frequency_when_input_is_a_dict_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let path = std::env::temp_dir().join("wind_en_head_indict_hit.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    // 让 `hello` 在调频上明显压过 `hell`，据此才测得出「原文候选不受调频影响」。
    for _ in 0..9 {
        store.record_freq("english", "hello", "hello").unwrap();
    }

    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::InDict;
    cfg.schema.english.frequency.enabled = true;
    let coord =
        Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(&store));
    type_word(&coord, "hell");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("hell"),
        "`hell` 是词库词 ⇒ InDict 档下应钉首位、不被 `hello` 的词频顶下去"
    );

    // 反向对照：同一份词频下，`Off` 档的首选就是被顶上来的 `hello`。缺了这条，
    // 把 InDict 实现成 Off 的别名也能让上面那句通过。
    let mut off = english_config();
    off.schema.english.raw_candidate = RawCandidateMode::Off;
    off.schema.english.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(off, Some(&data_dir()), Arc::clone(&store));
    type_word(&coord, "hell");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("hello"),
        "前提：Off 档下调频确实会把 `hello` 顶到 `hell` 之前（否则上一句测不出东西）"
    );
    let _ = std::fs::remove_file(&path);
}

/// 反向对照：`Always` 档下 `hel` 照旧钉在首位——InDict 不是 Always 的别名。
#[test]
fn always_still_pins_raw_even_when_not_a_dict_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::Always;
    let coord = coord_with(cfg, "always_miss");
    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("hel"),
        "Always 档的语义未变：不管词库有没有，原文恒是首候选"
    );
}

/// ★ **判据按字面，不忽略大小写**：打 `usa` 时词库里只有 `USA`，`usa` 这个字面不在词库里
/// ⇒ 不产原文候选，`USA` 自然成为首选（用户拍板的取舍；想要小写 `usa` 仍可按回车上屏原码）。
///
/// 这条是判据选型的守门用例：判据一旦放宽成 `eq_ignore_ascii_case`，首选会变回小写 `usa`。
#[test]
fn in_dict_compares_literally_so_lowercase_usa_yields_uppercase_dict_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::InDict;
    let coord = coord_with(cfg, "indict_usa");
    type_word(&coord, "usa");
    let page = coord.debug_page_texts();
    assert!(
        !page.contains(&"usa".to_string()),
        "词库里是 `USA`，小写 `usa` 的字面并不在词库中 ⇒ 不该产出原文候选（实际：{page:?}）"
    );
    assert_eq!(
        page.first().map(String::as_str),
        Some("USA"),
        "首选应是词库里的 `USA`（实际：{page:?}）"
    );
}

/// 两侧独立：临英那份 `InDict` 同样生效，且判据看的是**投影后**的候选文本。
///
/// 临英由 Shift+字母进入 ⇒ 缓冲首字母恒大写（`Hell`），而词库里是 `hell`。
/// `case_follow_input`（临英默认开）会把词库候选投影成 `Hell`，此时它才与所打原文字面
/// 相同。⚠️ 判据若在投影**之前**求值，这条就会红——那正是它要钉住的东西。
#[test]
fn in_dict_applies_to_temp_english_after_case_projection() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    // ⚠️ **必须借调频才分得出**：投影后的词库候选文本同样是 `Hell`，只断言「首选是 Hell」
    // 的话，判据放在投影前后都能过（实测变异确认过这条假绿）。真正的差别是**受不受调频
    // 影响**——头部候选钉最前，词库候选会被高频词顶下去。故让 `hello` 高频，再看 `Hell`
    // 还在不在首位。
    let path = std::env::temp_dir().join("wind_en_head_indict_temp_hit.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    for _ in 0..9 {
        store.record_freq("english", "hello", "hello").unwrap();
    }
    let mut cfg = temp_english_config();
    cfg.input.temp_english.raw_candidate = RawCandidateMode::InDict;
    cfg.input.temp_english.case_variants = false;
    cfg.schema.english.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    enter_temp_english(&coord, "hell");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("Hell"),
        "投影后 `hell`→`Hell` 与所打原文字面相同 ⇒ 应产出原文候选并钉首位，\
         压过高频的 `Hello`（判据若在投影前求值，这里会是 `Hello`）"
    );
    let _ = std::fs::remove_file(&path);

    let mut cfg = temp_english_config();
    cfg.input.temp_english.raw_candidate = RawCandidateMode::InDict;
    cfg.input.temp_english.case_variants = false;
    let coord = coord_with(cfg, "indict_temp_miss");
    enter_temp_english(&coord, "hel");
    let page = coord.debug_page_texts();
    assert!(
        !page.contains(&"Hel".to_string()),
        "`Hel` 不是词库词 ⇒ 临英侧同样不产原文候选（实际：{page:?}）"
    );
}

/// 配置兼容：老配置里 `raw_candidate` 是 `bool`，升级后必须逐字节同义。
///
/// ⚠️ `false` 那半边不可省：把兼容实现写成「认不出就当 Always」时，正是它单独变红。
/// 关掉过这一项的用户升级后突然多出一条原文候选，而他没改过任何设置。
#[test]
fn legacy_bool_config_still_parses() {
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default)]
        english: wind_config::config::EnglishGlobal,
    }
    let on: Probe =
        toml::from_str("[english]\nraw_candidate = true\n").expect("老配置 true 应可读");
    assert_eq!(on.english.raw_candidate, RawCandidateMode::Always);
    let off: Probe =
        toml::from_str("[english]\nraw_candidate = false\n").expect("老配置 false 应可读");
    assert_eq!(off.english.raw_candidate, RawCandidateMode::Off);
    let new: Probe =
        toml::from_str("[english]\nraw_candidate = \"in_dict\"\n").expect("新写法应可读");
    assert_eq!(new.english.raw_candidate, RawCandidateMode::InDict);
    // 值域外的字符串回落出厂档（同 `tolerant_de` 的值域层容错），不整段失效。
    let bad: Probe =
        toml::from_str("[english]\nraw_candidate = \"in_dcit\"\n").expect("写错的值应回落而非报错");
    assert_eq!(bad.english.raw_candidate, RawCandidateMode::Always);
}

/// ★ **`show_candidates = false` ⇒ 词库根本没被查询，`in_dict` 必须退回 `always`**。
///
/// 这个组合是 `in_dict` 引入的新坑：`overlay_engine_schema` 在候选关闭时返回 `None`
/// ⇒ 词库段恒空 ⇒ 若让判据直接作用于空表，它恒假；而 `want_variants` 同时也被
/// `dict_schema.is_some()` 摁成 false ⇒ **临英一条候选都不产**，用户在设置页选的
/// 「仅当它是词库里的词」实际表现成「永不显示」。
///
/// 判据在这里不是「问过但没命中」，是**根本没被问过**——两者必须区分开。本用例钉住
/// §5.5 那条既有语义：原文候选在候选关闭时仍是「空格上屏什么」的依据。
#[test]
fn in_dict_falls_back_to_always_when_dictionary_is_never_queried() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = temp_english_config();
    cfg.input.temp_english.raw_candidate = RawCandidateMode::InDict;
    cfg.input.temp_english.show_candidates = false;
    let coord = coord_with(cfg, "indict_no_dict");
    enter_temp_english(&coord, "hel");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("Hel"),
        "候选关闭 ⇒ 词库没被查询 ⇒ InDict 无从判断，应退回 Always 保住原文那条（实际：{page:?}）"
    );
}

/// `in_dict` 未命中 + 变形也关 + 词库无命中 ⇒ 候选为空时，空格仍须上屏输入串（§5.5 硬承诺）。
///
/// 既有的两条空候选用例只测了 `Off`。`in_dict` 未命中当前走同一段代码，但那是实现细节
/// ——一旦有人给 `InDict` 加特判就会静默失守，而这条承诺是「打了一串英文按空格什么都没
/// 发生」与否的分界。
#[test]
fn in_dict_miss_with_empty_candidates_still_commits_the_raw_input() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = english_config();
    cfg.schema.english.raw_candidate = RawCandidateMode::InDict;
    cfg.schema.english.case_variants = false;
    let coord = coord_with(cfg, "indict_empty");
    type_word(&coord, "zzzqx");
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：`zzzqx` 在英文词库里应无命中，且 InDict 未命中不产原文候选"
    );
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { text, .. } => assert!(
            text.starts_with("zzzqx"),
            "候选为空时空格必须上屏输入串，实际上屏：{text:?}"
        ),
        other => panic!("空候选时空格不得吞键，实际动作：{other:?}"),
    }
}
