//! A2-39：临时英文 / 快捷输入里用过的英文词，再打短前缀时要能靠词频浮上来（t202）。
//!
//! 反馈：打完 `translation` 上屏，再键入 `t`，`translation` 不置顶。英文调频
//! （`schema.english.frequency.enabled`）出厂关，那是出厂值问题、不在本文件；开了调频之后
//! 还有两个**代码**问题，本文件各钉一组：
//!
//! - **②「打全再上屏」一条都不记**：打全 `translation` 时词库那条被头部候选（原文
//!   `Translation` / 变形 `translation`）按字面去重吃掉，上屏的是头部候选——它没有词库来源，
//!   写端一律跳过。修法：头部候选上屏时按英文词库的键规则（小写码、大小写不敏感）找回它对应的
//!   词库词，按那个词记——与从列表里选中该词逐字节同一行。**不在词库里的纯原文仍不记**
//!   （`temp_english_freq.rs::temp_english_literal_text_is_not_recorded` 钉着）。
//! - **③ 短前缀下取不回来**：`t` 下引擎只取 300 条，`translation` 不在其中；词频重排只作用于
//!   已取回的候选，用多少次都排不上来。修法：调频开着时把词频表里以当前前缀开头、有记录、且
//!   仍是词库词的补进候选池（放在池尾，即「原序至少在池外」），再参与重排。
//!
//! 位次一律走 **Coordinator 实测**（`debug_all_candidate_texts` 是界面上的最终列表），
//! 不拿引擎 `convert` 的顺序——协调器还要叠头部候选、去重、重排。
//!
//! 反向对照（调频关）不可省：「召回」若不看开关，关着时 `t` 下就会多出一条，
//! 那是对出厂行为的静默改动。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族跳过。
//!
//! 用例里的观测词是 `tranquil` 而非反馈里的 `translation`，理由见 [`WORD`]。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

/// 观测词取一个**没有词频名次**的词库词：英文候选按词频名次排序后，有名次的常用词
/// （如反馈里的 `translation`）已在 `t` 的 300 条池内，测不出「池外召回」。无名次的词排在
/// 所有有名次的词之后（`t` 下有名次的有八百多条），必在池外。
const WORD: &str = "tranquil";
const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_SEMICOLON: u32 = 0xBA;

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

/// 主方案取五笔（临英 / 快捷输入的归属必须是英文桶，与 active 无关）。临英的原文 / 变形 /
/// 大小写投影保持 `Config::default()` 的出厂值（都开着）——反馈就发生在这组值下。
fn config(freq: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.enabled = true;
    cfg.schema.english.frequency.enabled = freq;
    cfg
}

fn fresh_store(tag: &str) -> (Arc<Store>, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "wind_a2_39_recall_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    (Arc::new(Store::open(&path).unwrap()), path)
}

/// Shift+首字母进临英，再打剩余字母（临英缓冲首字母恒大写）。
fn enter_temp_english(coord: &Coordinator, word: &str) {
    let mut chars = word.chars();
    let first = chars.next().unwrap();
    coord.handle_key_event(&key((first.to_ascii_uppercase() as u32) & 0xFF, MOD_SHIFT));
    for c in chars {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

fn enter_quick(coord: &Coordinator, word: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
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

/// 界面最终列表里 `WORD`（任意大小写形）的下标。出厂 `case_follow_input` 开着，临英缓冲
/// 首字母恒大写 ⇒ 词库候选被投影成 `Tranquil`，故忽略大小写比。
fn rank_of_word(coord: &Coordinator) -> Option<usize> {
    coord
        .debug_all_candidate_texts()
        .iter()
        .position(|t| t.eq_ignore_ascii_case(WORD))
}

fn english_count(store: &Store, code: &str, text: &str) -> u32 {
    store
        .get_freq("english", code, text)
        .unwrap()
        .map_or(0, |r| r.count)
}

/// 临英打全 `tranquil` 按空格：上屏的是首条原文 `Tranquil`（出厂 `raw_candidate = always`）。
fn full_space(coord: &Coordinator) {
    enter_temp_english(coord, WORD);
    let text = commit_text(&coord.handle_key_event(&key(VK_SPACE, 0)));
    assert_eq!(text.trim_end(), "Tranquil", "前提：空格上屏的是首条原文");
}

// ───────────────────────── ② 头部候选上屏按词库词记 ─────────────────────────

/// 打全按空格（原文 `Tranquil`）→ 记成词库词 `(tranquil, tranquil)`，
/// 与从列表里选中词库那条是同一行。
#[test]
fn raw_head_commit_credits_the_dict_word() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let (store, path) = fresh_store("raw_head");
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    full_space(&coord);
    assert_eq!(
        english_count(&store, WORD, WORD),
        1,
        "原文 `Tranquil` 对应词库词 `tranquil`，应按它记一次"
    );
    // 不得另记一条按原文形态的孤儿键（读端永远查不中）。
    let mut rows = Vec::new();
    store
        .for_each_freq("english", "", &mut |code, text, rec| {
            rows.push(format!("({code}, {text}, {})", rec.count));
            true
        })
        .unwrap();
    assert_eq!(
        rows,
        vec![format!("({WORD}, {WORD}, 1)")],
        "英文桶里应恰好一行"
    );
    let _ = std::fs::remove_file(&path);
}

/// 打全选小写变形那条（`tranquil`，头部变形而非词库候选）→ 同样记成词库词。
#[test]
fn case_variant_commit_credits_the_dict_word() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("variant_head");
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    enter_temp_english(&coord, WORD);
    let page = coord.debug_page_texts();
    let p = page
        .iter()
        .position(|t| t == WORD)
        .unwrap_or_else(|| panic!("前提：页上应有小写变形 `{WORD}`，实际: {page:?}"));
    commit_text(&coord.handle_key_event(&key(0x31 + p as u32, 0)));
    assert_eq!(english_count(&store, WORD, WORD), 1);
    let _ = std::fs::remove_file(&path);
}

/// 调频关：头部候选上屏照旧一条不记（开关语义不变）。
#[test]
fn raw_head_commit_records_nothing_when_freq_off() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("raw_head_off");
    let coord =
        Coordinator::new_headless_with_store(config(false), Some(&data_dir()), store.clone());
    full_space(&coord);
    assert_eq!(english_count(&store, WORD, WORD), 0);
    let _ = std::fs::remove_file(&path);
}

/// 快捷输入英文成员的头部候选（`;tranquil` 里的原文 `tranquil`）上屏 → 记进**英文桶**，
/// 不再落到主方案桶。
///
/// 改前它没有来源 ⇒ 反查不到成员方案 ⇒ 归属落回 active（五笔），码表调频开着时写出
/// `("wubi86", "tranquil", "tranquil")` 这条五笔读端永远用不上的行。故这里特意打开
/// 码表调频，才看得见那条错行。
#[test]
fn quick_input_head_commit_credits_english_bucket() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("quick_head");
    let mut cfg = config(true);
    cfg.schema.codetable.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    enter_quick(&coord, WORD);
    let all = coord.debug_all_candidate_texts();
    let i = all
        .iter()
        .position(|t| t == WORD)
        .unwrap_or_else(|| panic!("前提：`;{WORD}` 应有原文候选，实际: {all:?}"));
    assert!(
        coord.select_candidate(i).consumed,
        "选中快捷输入的原文候选应上屏"
    );
    assert_eq!(english_count(&store, WORD, WORD), 1, "应记进英文桶");
    let mut wubi_rows = 0;
    store
        .for_each_freq("wubi86", "", &mut |_, _, _| {
            wubi_rows += 1;
            true
        })
        .unwrap();
    assert_eq!(wubi_rows, 0, "英文头部候选不得记进主方案（五笔）桶");
    let _ = std::fs::remove_file(&path);
}

// ───────────────────────── ③ 短前缀召回 ─────────────────────────

/// 端到端：临英打全 `tranquil` 空格上屏 5 次，再键入 `t`——它进入候选并靠前。
///
/// 位次依据（英文出厂 `strategy = position`、`promote_prefix = all`）：召回的词放在词库段
/// **池尾**（`t` 下池子 300 条 ⇒ base_pos ≈ 300），5 次有效使用 ⇒ 目标位次
/// `300 / 2^5 ≈ 9`（衰减让有效次数略小于 5，floor 后仍是 9）；同位次时有词频者在前。
/// 词库段之前还有头部候选（`T` 原文 + `t` 变形，2 条），合计界面下标 ≤ 11
/// （2026-09-25 实测恰为 11：第 2 页第 5 位，每页 7 条）。
#[test]
fn used_word_is_recalled_under_single_letter() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("recall_t");
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    enter_temp_english(&coord, "t");
    assert_eq!(
        rank_of_word(&coord),
        None,
        "前提：未用过时 `t` 下取不回 {WORD}（否则本条测不出召回）"
    );
    coord.handle_key_event(&key(VK_ESCAPE, 0));

    for _ in 0..5 {
        full_space(&coord);
    }
    assert_eq!(english_count(&store, WORD, WORD), 5, "前提：② 记满 5 次");

    enter_temp_english(&coord, "t");
    let r = rank_of_word(&coord);
    let all = coord.debug_all_candidate_texts();
    assert!(
        r.is_some_and(|i| i <= 11),
        "用过 5 次后 `t` 下 {WORD} 应被召回并排进前 12 条，实际下标 {r:?}，前 15 条: {:?}",
        &all[..all.len().min(15)]
    );
    let _ = std::fs::remove_file(&path);
}

/// 调频关：同样上屏 5 次，`t` 下的整张列表与从没用过时**逐字相同**。
#[test]
fn freq_off_leaves_single_letter_list_unchanged() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("recall_off");
    // 词频表里即便有记录（例如调频开过又关了），关着时也不得召回——直接种记录来钉这一点。
    for _ in 0..5 {
        store.record_freq("english", WORD, WORD).unwrap();
    }
    let coord =
        Coordinator::new_headless_with_store(config(false), Some(&data_dir()), store.clone());
    enter_temp_english(&coord, "t");
    let before = coord.debug_all_candidate_texts();
    coord.handle_key_event(&key(VK_ESCAPE, 0));
    for _ in 0..5 {
        full_space(&coord);
    }
    enter_temp_english(&coord, "t");
    let after = coord.debug_all_candidate_texts();
    assert_eq!(before, after, "调频关时 `t` 下的列表不得变化");
    assert!(
        !after.iter().any(|t| t.eq_ignore_ascii_case(WORD)),
        "调频关时不得召回 {WORD}"
    );
    let _ = std::fs::remove_file(&path);
}

/// 召回只收**仍是词库词**的记录，且验证的是 `(码, 文本)` 这一整条：种一条「码是真实词库码
/// （`thoughtless`）、文本却不是词库词（`thoughtlessx`）」的记录（导入残留 / 词库改过文本），`t` 下的
/// 列表必须与没有任何记录时逐字相同——只按码验证的话，会把码对应的词库词 `thoughtless` 召回来，
/// 借一条不属于它的记录冒出来。词频是排序维度，不是词条来源。
#[test]
fn recall_ignores_records_that_are_not_dict_words() {
    if !has_english_schema() {
        return;
    }
    let (empty, empty_path) = fresh_store("recall_nondict_base");
    let coord = Coordinator::new_headless_with_store(config(true), Some(&data_dir()), empty);
    enter_temp_english(&coord, "t");
    let baseline = coord.debug_all_candidate_texts();
    assert!(
        !baseline
            .iter()
            .any(|t| t.eq_ignore_ascii_case("thoughtless")),
        "前提：`thoughtless` 不在 `t` 的原始池里（否则测不出误召回）"
    );

    let (store, path) = fresh_store("recall_nondict");
    for _ in 0..5 {
        store
            .record_freq("english", "thoughtless", "thoughtlessx")
            .unwrap();
    }
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    enter_temp_english(&coord, "t");
    assert_eq!(
        coord.debug_all_candidate_texts(),
        baseline,
        "码是词库码、文本不是词库词的记录不得召回任何东西"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&empty_path);
}

/// 召回在**快捷输入的英文成员**与**英文方案**上同样生效（读端直接种 5 次记录）。
///
/// 快捷输入英文段在拼音成员之后，只断言「进了列表」（英文段截到配额 50 条之后仍在，
/// 即重排发生在截配额之前）；英文方案下与临英同一把尺：头部只有原文 `t` 1 条（英文方案
/// 出厂不出变形）+ 词库段 `300 / 2^5` ⇒ 第 9 ⇒ 界面下标 ≤ 10。
#[test]
fn recall_reaches_quick_input_and_english_schema() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("recall_members");
    for _ in 0..5 {
        store.record_freq("english", WORD, WORD).unwrap();
    }

    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    enter_quick(&coord, "t");
    assert!(
        rank_of_word(&coord).is_some(),
        "快捷输入 `;t` 的英文段应召回 {WORD}"
    );

    let mut cfg = config(true);
    cfg.schema.active = "english".into();
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    coord.handle_key_event(&key(b'T' as u32, 0));
    let r = rank_of_word(&coord);
    assert!(
        r.is_some_and(|i| i <= 10),
        "英文方案 `t` 下应召回 {WORD} 并排进前 11 条，实际 {r:?}"
    );

    // 反向对照：英文方案调频关时不召回。
    let mut off = config(false);
    off.schema.active = "english".into();
    let coord = Coordinator::new_headless_with_store(off, Some(&data_dir()), store.clone());
    coord.handle_key_event(&key(b'T' as u32, 0));
    assert_eq!(rank_of_word(&coord), None, "英文方案调频关时不得召回");
    let _ = std::fs::remove_file(&path);
}

// ───────────────────────── 审查补充 ─────────────────────────

const VK_COMMA: u32 = 0xBC;

/// 快捷输入里 `;` 空缓冲时的唯一候选＝上屏历史的最近一条（重复上屏）。
fn repeat_candidate(coord: &Coordinator) -> Vec<String> {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    let page = coord.debug_page_texts();
    coord.handle_key_event(&key(VK_ESCAPE, 0));
    page
}

/// M1：临英上屏历史记**实际上屏文本**，与词频分开。打 `Tranquil` 空格上屏后，`;` 重复
/// 上屏调回来的必须是 `Tranquil`，不是词频键用的词库原文 `tranquil`。
#[test]
fn temp_english_history_keeps_committed_case() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("history_on");
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    full_space(&coord);
    assert_eq!(repeat_candidate(&coord), vec!["Tranquil".to_string()]);
    assert_eq!(english_count(&store, WORD, WORD), 1, "词频照记词库原文");
    let _ = std::fs::remove_file(&path);
}

/// M1 反向：调频关时历史照记（历史是重复上屏的数据源，与调频开关无关）。
#[test]
fn temp_english_history_recorded_when_freq_off() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("history_off");
    let coord =
        Coordinator::new_headless_with_store(config(false), Some(&data_dir()), store.clone());
    full_space(&coord);
    assert_eq!(repeat_candidate(&coord), vec!["Tranquil".to_string()]);
    assert_eq!(english_count(&store, WORD, WORD), 0, "调频关不记词频");
    let _ = std::fs::remove_file(&path);
}

/// L4：英文方案主输入路，大写打 `Tranquil` 空格上屏（头部原文）→ 记到词库词
/// `(tranquil, tranquil)`，不写 `(tranquil, Tranquil)` 这条孤儿键。
#[test]
fn english_schema_cased_raw_commit_credits_dict_word() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("schema_raw");
    let mut cfg = config(true);
    cfg.schema.active = "english".into();
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    coord.handle_key_event(&key(b'T' as u32, MOD_SHIFT));
    for c in WORD[1..].chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    let text = commit_text(&coord.handle_key_event(&key(VK_SPACE, 0)));
    assert_eq!(text.trim_end(), "Tranquil", "前提：空格上屏的是头部原文");
    assert_eq!(english_count(&store, WORD, WORD), 1, "应记到词库词");
    assert_eq!(
        english_count(&store, WORD, "Tranquil"),
        0,
        "不得记按原文形态的孤儿键"
    );
    let _ = std::fs::remove_file(&path);
}

/// T1：快捷输入 `;tranquil` 高亮原文后按 `,` 顶屏——英文桶记一次、五笔桶为空。
/// 成员只留 english，使首条（高亮）就是英文原文；码表调频打开才看得见落错桶。
#[test]
fn quick_input_punct_commit_of_head_credits_english_bucket() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("quick_punct");
    let mut cfg = config(true);
    cfg.schema.codetable.frequency.enabled = true;
    cfg.schema.mix_modes[0].members.retain(|m| m == "english");
    // 出厂 `free_input = auto` 下 `,` 是越界字符、会作字面进缓冲，走不到顶屏臂。
    cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    enter_quick(&coord, WORD);
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(WORD),
        "前提：首条是英文原文"
    );
    let text = commit_text(&coord.handle_key_event(&key(VK_COMMA, 0)));
    assert!(text.starts_with(WORD), "顶屏应上屏原文，实际 {text:?}");
    assert_eq!(english_count(&store, WORD, WORD), 1, "应记进英文桶");
    let mut wubi_rows = 0;
    store
        .for_each_freq("wubi86", "", &mut |_, _, _| {
            wubi_rows += 1;
            true
        })
        .unwrap();
    assert_eq!(wubi_rows, 0, "不得记进五笔桶");
    let _ = std::fs::remove_file(&path);
}

/// T2：词已在取回的池里时不重复召回——打 `tran`（`tranquil` 本就在池内）列表里它只出现
/// 一次。关掉大小写投影：投影开着时列表末尾那道去重会把重复条悄悄吃掉，测不出召回自己
/// 有没有排除池内的词。
#[test]
fn recall_does_not_duplicate_words_already_in_pool() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("no_dup");
    for _ in 0..5 {
        store.record_freq("english", WORD, WORD).unwrap();
    }
    let mut cfg = config(true);
    cfg.input.temp_english.case_follow_input = false;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    enter_temp_english(&coord, "tran");
    let all = coord.debug_all_candidate_texts();
    assert_eq!(
        all.iter().filter(|t| t.as_str() == WORD).count(),
        1,
        "{WORD} 应恰好出现一次: {:?}",
        &all[..all.len().min(15)]
    );
    let _ = std::fs::remove_file(&path);
}

/// 30 个 `t` 开头的出厂词库词（多数原序在 `t` 的 300 条池外）。
const T_WORDS: &[&str] = &[
    "tabulate",
    "tangible",
    "teaching",
    "technique",
    "telecoms",
    "television",
    "temporary",
    "terminals",
    "terrific",
    "testified",
    "thankful",
    "theories",
    "thickness",
    "thousands",
    "thresholds",
    "thumbnails",
    "timeshare",
    "toilette",
    "tortured",
    "toxicity",
    "trademarks",
    "trainers",
    "transcript",
    "transfers",
    "transistor",
    "transparent",
    "traumatic",
    "traverse",
    "treating",
    "tackling",
];

/// L1-a：**先验证、后截断**。同前缀下有 20 条次数更高的非词库记录时，用了 5 次的词库词
/// `tranquil` 仍要召回——先截前 16 条再验证的话，名额全被非词库残留占掉。
#[test]
fn recall_validates_before_truncating() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("validate_first");
    for i in 0..20 {
        let junk = format!("tzzq{i:02}");
        for _ in 0..10 {
            store.record_freq("english", &junk, &junk).unwrap();
        }
    }
    for _ in 0..5 {
        store.record_freq("english", WORD, WORD).unwrap();
    }
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    enter_temp_english(&coord, "t");
    let r = rank_of_word(&coord);
    assert!(
        r.is_some_and(|i| i <= 11),
        "非词库记录不得挤掉 {WORD}，实际下标 {r:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// L1-b：召回按**衰减后的有效次数**排，与重排同一把尺。30 条一年前用过 50 次的旧记录
/// （衰减殆尽，重排里提升强度为 0）不得挤掉最近用了 5 次的 `tranquil`——按原始次数排
/// 的话旧记录全排在前面、占满名额。
#[test]
fn recall_ranks_by_decayed_count() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("decayed");
    let year_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 365 * 24 * 3600;
    let jsonl: String = T_WORDS
        .iter()
        .map(|w| {
            format!("{{\"code\":\"{w}\",\"text\":\"{w}\",\"count\":50,\"last_used\":{year_ago}}}\n")
        })
        .collect();
    let (imported, _) = store.import_freq_jsonl("english", &jsonl).unwrap();
    assert_eq!(imported, T_WORDS.len(), "前提：旧记录全部导入");
    for _ in 0..5 {
        store.record_freq("english", WORD, WORD).unwrap();
    }
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    enter_temp_english(&coord, "t");
    let r = rank_of_word(&coord);
    assert!(
        r.is_some_and(|i| i <= 11),
        "衰减殆尽的旧记录不得挤掉 {WORD}，实际下标 {r:?}"
    );
    let _ = std::fs::remove_file(&path);
}

// ───────────────────────── 英文方案：非词库原文不记词频 ─────────────────────────

/// 英文方案（调频开）下打一个词库里没有的串，高亮第 `pick` 条上屏；返回上屏文本。
fn english_schema_commit(coord: &Coordinator, word: &str, pick: usize) -> String {
    for c in word.chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    let act = if pick == 0 {
        coord.handle_key_event(&key(VK_SPACE, 0))
    } else {
        coord.handle_key_event(&key(0x31 + pick as u32, 0))
    };
    commit_text(&act)
}

fn english_rows(store: &Store) -> Vec<String> {
    let mut rows = Vec::new();
    store
        .for_each_freq("english", "", &mut |code, text, rec| {
            rows.push(format!("({code}, {text}, {})", rec.count));
            true
        })
        .unwrap();
    rows
}

fn english_schema_config() -> Config {
    let mut cfg = config(true);
    cfg.schema.active = "english".into();
    cfg
}

/// 英文方案下空格上屏**非词库原文**（`wxyzq`，头部原文候选、找不到对应词库词）：
/// **不记词频**（读端永远查不中的孤儿键，与临英 / 快捷输入同口径），但输入统计与上屏历史
/// 照记——`;` 进快捷输入，重复上屏候选就是它。
#[test]
fn english_schema_literal_raw_commit_records_no_freq() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("schema_literal");
    let coord = Coordinator::new_headless_with_store(
        english_schema_config(),
        Some(&data_dir()),
        store.clone(),
    );
    coord.debug_capture_stat_events();
    let text = english_schema_commit(&coord, "wxyzq", 0);
    assert_eq!(text.trim_end(), "wxyzq", "前提：空格上屏的是原文");
    assert_eq!(
        english_rows(&store),
        Vec::<String>::new(),
        "非词库原文不得记词频"
    );
    assert!(
        coord
            .debug_take_stat_events()
            .iter()
            .any(|(_, t)| t == "wxyzq"),
        "输入统计照记"
    );
    // 上屏历史：`;` 进快捷输入，空缓冲的重复上屏候选就是它。
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_page_texts(),
        vec!["wxyzq".to_string()],
        "上屏历史照记：`;` 应调出 wxyzq"
    );
    let _ = std::fs::remove_file(&path);
}

/// 对照：**用户词库里的词**（`wxyzqab`，英文来源）照常记词频——它是词库候选，不是头部原文。
/// 同时覆盖用户词被头部原文按字面吃掉的情形（打全 `wxyzqab` 空格上屏原文）：能找到对应的
/// 用户词，按它记。
#[test]
fn english_schema_user_word_still_records_freq() {
    if !has_english_schema() {
        return;
    }
    let (store, path) = fresh_store("schema_userword");
    store
        .add_user_word("english", "wxyzqab", "wxyzqab", 0, 0)
        .unwrap();
    let coord = Coordinator::new_headless_with_store(
        english_schema_config(),
        Some(&data_dir()),
        store.clone(),
    );
    // 打 `wxyzq` 选第 2 条（首条是原文 `wxyzq`，其后是用户词 `wxyzqab`）。
    let text = english_schema_commit(&coord, "wxyzq", 1);
    assert_eq!(text.trim_end(), "wxyzqab", "前提：第 2 条是用户词");
    assert_eq!(english_count(&store, "wxyzqab", "wxyzqab"), 1);
    // 打全再空格上屏头部原文：按对应的用户词记。
    let text = english_schema_commit(&coord, "wxyzqab", 0);
    assert_eq!(text.trim_end(), "wxyzqab");
    assert_eq!(english_count(&store, "wxyzqab", "wxyzqab"), 2);
    let _ = std::fs::remove_file(&path);
}
