//! 英文「屏蔽这个词」：英文方案 / 临时英文右键删 ≥2 字母的词，按**词库原文**屏蔽（`english` 桶里
//! `code = *` 的删除行），所有前缀、补全都不再出；输入原文那一条不受屏蔽、回车照常上屏。
//! 大小写不归一（`ok` 与 `OK` 是两个词）。混在拼音 / 码表 / 混输里的英文不读这个屏蔽集。
//! 另含英文删除 / 置顶在大小写跟随下的落键修正、混入英文的用户词删除。
//! 设计见 docs/design/system-dict-patch-and-candidate-adjust.md §4a、§8 第 11 条。
//!
//! 每条用例先断言删除前候选确实在。词库候选与头部原文候选靠编码区分：头部候选无码。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族静默跳过（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;
use wind_ui_types::{CandidateOp, MenuKind};

const VK_RETURN: u32 = 0x0D;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "english", "pinyin", "wubi86_pinyin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
        && d.join("schemas/english/en.dict.yaml").exists()
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

fn key(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 逐键敲入；大写字母带 Shift（五笔下首字母大写即进临时英文）。
fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let m = if ch.is_ascii_uppercase() {
            MOD_SHIFT
        } else {
            0
        };
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF, m));
    }
}

/// `active`：`english` = 英文方案主路；`wubi86` = 五笔下用临时英文；也可是拼音 / 混输。
fn config(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec![
        "wubi86".into(),
        "english".into(),
        "pinyin".into(),
        "wubi86_pinyin".into(),
    ];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.keys.select_key_groups.clear();
    c
}

fn fresh_store(name: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_enblock_{name}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

fn open(cfg: &Config, store: &Arc<Store>) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store))
}

/// 打 `input`，返回全部候选 `(文本, 编码)`。
fn triples(cfg: &Config, store: &Arc<Store>, input: &str) -> Vec<(String, String)> {
    let coord = open(cfg, store);
    type_str(&coord, input);
    coord
        .debug_candidate_triples()
        .into_iter()
        .map(|(t, c, _)| (t, c))
        .collect()
}

/// 有没有文本（忽略大小写）等于 `word` 的**词库**候选（有编码；头部原文候选无码）。
fn has_dict_word(cands: &[(String, String)], word: &str) -> bool {
    cands
        .iter()
        .any(|(t, c)| !c.is_empty() && t.eq_ignore_ascii_case(word))
}

/// 打 `input`，右键删首页上文本为 `want` 的候选。
fn delete_on_page(cfg: &Config, store: &Arc<Store>, input: &str, want: &str) {
    let coord = open(cfg, store);
    type_str(&coord, input);
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == want)
        .unwrap_or_else(|| panic!("前提：`{input}` 首页应有「{want}」，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
}

fn blocked(store: &Store, schema: &str) -> Vec<String> {
    let mut v: Vec<String> = store
        .blocked_words(schema)
        .unwrap()
        .iter()
        .cloned()
        .collect();
    v.sort();
    v
}

fn delete_item(coord: &Coordinator, idx: usize) -> wind_ui_types::MenuItemSpec {
    coord
        .candidate_menu_items(idx)
        .unwrap()
        .into_iter()
        .find(|m| matches!(m.kind, MenuKind::Op(CandidateOp::Delete)))
        .expect("菜单应有删除项")
}

/// ★ 临英 `Hel` 下删投影成 `Help` 的词库词 `help`：按词库原文屏蔽，`he` / `help` / `helper`
/// 下都不再出这个词库词，英文方案主路同样不出；`Help` 原文那条仍在、回车照常上屏。
#[test]
fn temp_english_delete_blocks_word_by_dict_text() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("temp_hel");
    for input in ["He", "Help"] {
        assert!(
            has_dict_word(&triples(&cfg, &store, input), "help"),
            "前提：临英 `{input}` 有词库词 help"
        );
    }
    let coord = open(&cfg, &store);
    type_str(&coord, "Hel");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == "Help")
        .expect("前提：首页有 Help");
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    let now = coord.debug_page_texts();
    assert_eq!(
        now.first().map(String::as_str),
        Some("Hel"),
        "当场重建，原文条仍在：{now:?}"
    );
    assert!(!now.iter().any(|t| t == "Help"), "当场不再出：{now:?}");
    assert_eq!(blocked(&store, "english"), vec!["help".to_string()]);
    assert!(
        store.get_shadow_rules("english", "hel").unwrap().is_none(),
        "不写按编码的删除行"
    );
    for input in ["He", "Hel", "Help", "Helpe"] {
        let after = triples(&cfg, &store, input);
        assert!(
            !has_dict_word(&after, "help"),
            "临英 `{input}` 不应再有词库词 help，实际 {after:?}"
        );
    }
    let main = config("english");
    for input in ["he", "help"] {
        let after = triples(&main, &store, input);
        assert!(
            !has_dict_word(&after, "help"),
            "英文方案 `{input}` 同样不出，实际 {after:?}"
        );
    }
    assert!(
        has_dict_word(&triples(&main, &store, "helpe"), "helper"),
        "以它为前缀的别的词不受影响"
    );
    // 原文那条不受屏蔽：钉首位、回车上屏
    let coord = open(&cfg, &store);
    type_str(&coord, "Help");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("Help")
    );
    match coord.handle_key_event_policed(&key(VK_RETURN, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), "Help"),
        other => panic!("回车应上屏原文，实际 {other:?}"),
    }
}

/// `raw_candidate = in_dict`：屏蔽的词仍是「词库里的词」，原文那条照样出、照样在首位。
#[test]
fn in_dict_raw_candidate_survives_block() {
    skip_without_data!();
    let mut cfg = config("english");
    cfg.schema.english.raw_candidate = wind_config::config::RawCandidateMode::InDict;
    let store = fresh_store("in_dict");
    let before = triples(&cfg, &store, "help");
    assert_eq!(before.first().map(|c| c.0.as_str()), Some("help"), "前提");
    store.block_word("english", "help").unwrap();
    let coord = open(&cfg, &store);
    type_str(&coord, "help");
    let page = coord.debug_candidate_triples();
    assert_eq!(
        page.first().map(|c| (c.0.as_str(), c.1.as_str())),
        Some(("help", "")),
        "原文条仍在首位（无码的头部候选），实际 {page:?}"
    );
    assert!(!page.iter().skip(1).any(|c| c.0 == "help"));
    match coord.handle_key_event_policed(&key(0x20, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), "help"),
        other => panic!("空格应上屏原文，实际 {other:?}"),
    }
}

/// CapsLock 全大写档下删 `HELP`：键仍是词库原文 `help`。
#[test]
fn capslock_upper_variant_delete_uses_dict_text() {
    skip_without_data!();
    let mut cfg = config("wubi86");
    cfg.input.english_case_cycle_key = "capslock".into();
    let store = fresh_store("caps");
    let coord = open(&cfg, &store);
    type_str(&coord, "Hel");
    assert!(coord.debug_cycle_english_case(), "前提：切到全大写档");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == "HELP")
        .unwrap_or_else(|| panic!("前提：全大写档首页应有 HELP，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    assert_eq!(blocked(&store, "english"), vec!["help".to_string()]);
    assert!(!has_dict_word(&triples(&cfg, &store, "Hel"), "help"));
}

/// 大小写不归一：删 `OK` 不连带 `ok`，反之亦然。
#[test]
fn block_is_case_sensitive() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("ok");
    let before = triples(&cfg, &store, "ok");
    assert!(
        before.iter().any(|(t, c)| t == "OK" && !c.is_empty()),
        "前提：{before:?}"
    );
    assert!(
        before.iter().any(|(t, c)| t == "ok" && !c.is_empty()),
        "前提：{before:?}"
    );
    delete_on_page(&cfg, &store, "ok", "OK");
    assert_eq!(blocked(&store, "english"), vec!["OK".to_string()]);
    let after = triples(&cfg, &store, "ok");
    assert!(!after.iter().any(|(t, _)| t == "OK"), "{after:?}");
    assert!(
        after.iter().any(|(t, c)| t == "ok" && !c.is_empty()),
        "`ok` 不受影响：{after:?}"
    );
}

/// 单字母不按文字屏蔽：单字母用户词只按编码真删（出厂词库没有单字母词条）。
#[test]
fn single_letter_not_blocked() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("single");
    store.add_user_word("english", "x", "X", 0, 0).unwrap();
    store.add_user_word("english", "xx", "X", 0, 0).unwrap();
    let coord = open(&cfg, &store);
    type_str(&coord, "x");
    let page = coord.debug_candidate_triples();
    let idx = page
        .iter()
        .position(|(t, c, _)| t == "X" && !c.is_empty())
        .unwrap_or_else(|| panic!("前提：`x` 应有用户词 X，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    assert!(blocked(&store, "english").is_empty(), "单字母不按文字屏蔽");
    assert!(store.get_user_words("english", "x").unwrap().is_empty());
    assert_eq!(
        store.get_user_words("english", "xx").unwrap().len(),
        1,
        "别的编码不动"
    );
}

/// ★ 英文方案主路 + 大小写跟随：`Hel` 下把投影成 `Held` 的词置顶，规则按词库原文落键、当场生效，
/// 菜单的「恢复默认」随之可用。修前写的是 `Held`、读端比的是 `held`，置顶无效。
#[test]
fn english_pin_under_case_follow_takes_effect() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("pin");
    let coord = open(&cfg, &store);
    type_str(&coord, "Hel");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == "Held")
        .unwrap_or_else(|| panic!("前提：首页应有 Held，实际 {page:?}"));
    assert!(idx > 1, "前提：Held 不在原文之后的第一格，实际 {page:?}");
    coord.debug_candidate_op(CandidateOp::MoveTop, idx);
    let rec = store.get_shadow_rules("english", "hel").unwrap().unwrap();
    assert_eq!(rec.pinned[0].word, "held", "规则键是词库原文");
    let coord = open(&cfg, &store);
    type_str(&coord, "Hel");
    let page = coord.debug_page_texts();
    assert_eq!(
        &page[..2],
        ["Hel", "Held"],
        "原文之后即置顶的词，实际 {page:?}"
    );
    let reset = coord
        .candidate_menu_items(1)
        .unwrap()
        .into_iter()
        .find(|m| matches!(m.kind, MenuKind::Op(CandidateOp::Reset)))
        .unwrap();
    assert!(reset.enabled, "置顶过的词「恢复默认」可用");
}

/// 头部候选（无码）只在字面就是所打原文、且词库有字面相同的词时才可删；单独出现的原文条
/// （`wxyzqk`；`usa`，词库只有 `USA`）与大小写变形条一律不可删：菜单灰掉、热键不写规则。
#[test]
fn head_raw_candidate_delete() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("head");
    for input in ["wxyzqk", "usa"] {
        let coord = open(&cfg, &store);
        type_str(&coord, input);
        let page = coord.debug_candidate_triples();
        assert_eq!(
            page.first().map(|c| (c.0.as_str(), c.1.as_str())),
            Some((input, "")),
            "前提：首位是无码的原文条，实际 {page:?}"
        );
        assert!(
            !delete_item(&coord, 0).enabled,
            "`{input}` 原文条：删除灰掉"
        );
        coord.debug_candidate_op(CandidateOp::Delete, 0);
    }
    assert!(
        store.list_shadow_rules("english").unwrap().is_empty(),
        "热键路径也不写规则"
    );
    assert!(
        has_dict_word(&triples(&cfg, &store, "usa"), "USA"),
        "USA 照出"
    );
}

/// 删了 `ok` 后，`ok` 的原文条单独出现：删它不得屏蔽同码的 `OK`。
#[test]
fn head_raw_after_block_does_not_hit_other_case() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("ok_head");
    let coord = open(&cfg, &store);
    type_str(&coord, "ok");
    let page = coord.debug_candidate_triples();
    assert_eq!(
        page.first().map(|c| (c.0.as_str(), c.1.as_str())),
        Some(("ok", "ok")),
        "前提：词库的 ok 占着原文那一格，实际 {page:?}"
    );
    coord.debug_candidate_op(CandidateOp::Delete, 0);
    assert_eq!(blocked(&store, "english"), vec!["ok".to_string()]);
    let coord = open(&cfg, &store);
    type_str(&coord, "ok");
    let page = coord.debug_candidate_triples();
    assert_eq!(
        page.first().map(|c| (c.0.as_str(), c.1.as_str())),
        Some(("ok", "")),
        "屏蔽后首位是无码的原文条，实际 {page:?}"
    );
    assert!(!delete_item(&coord, 0).enabled);
    coord.debug_candidate_op(CandidateOp::Delete, 0);
    assert_eq!(
        blocked(&store, "english"),
        vec!["ok".to_string()],
        "OK 不被连带屏蔽"
    );
    assert!(triples(&cfg, &store, "ok").iter().any(|(t, _)| t == "OK"));
}

/// 不跟随大小写时打 `Help`：原文条 `Help` 与词库行 `help` 分列，删原文条不得屏蔽 `help`。
#[test]
fn head_raw_does_not_hit_lower_dict_row() {
    skip_without_data!();
    let mut cfg = config("english");
    cfg.schema.english.case_follow_input = false;
    let store = fresh_store("help_head");
    let coord = open(&cfg, &store);
    type_str(&coord, "Help");
    let page = coord.debug_candidate_triples();
    assert_eq!(
        page.first().map(|c| (c.0.as_str(), c.1.as_str())),
        Some(("Help", "")),
        "前提：首位是无码的原文条，实际 {page:?}"
    );
    assert!(
        page.iter().any(|(t, c, _)| t == "help" && !c.is_empty()),
        "前提：{page:?}"
    );
    assert!(!delete_item(&coord, 0).enabled);
    coord.debug_candidate_op(CandidateOp::Delete, 0);
    assert!(blocked(&store, "english").is_empty());
}

/// 临英的大小写变形条（≠ 所打原文的头部候选）一律不可删：它对应不到唯一一个词库词。
#[test]
fn case_variant_head_not_deletable() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("variant");
    let coord = open(&cfg, &store);
    type_str(&coord, "Us");
    let page = coord.debug_candidate_triples();
    let variants: Vec<usize> = page
        .iter()
        .enumerate()
        .filter(|(_, (t, c, _))| c.is_empty() && t != "Us" && t.eq_ignore_ascii_case("us"))
        .map(|(i, _)| i)
        .collect();
    assert!(
        !variants.is_empty(),
        "前提：临英 `Us` 有变形条，实际 {page:?}"
    );
    for i in variants {
        assert!(
            !delete_item(&coord, i).enabled,
            "变形条 {:?} 不可删",
            page[i]
        );
        coord.debug_candidate_op(CandidateOp::Delete, i);
        assert_eq!(
            coord.debug_candidate_triples(),
            page,
            "临英里操作后按临英重建候选，窗口不被清空"
        );
    }
    assert!(blocked(&store, "english").is_empty());
    assert!(store.list_shadow_rules("english").unwrap().is_empty());
}

/// 英文用户词：真删所有编码条目再屏蔽；单条显式加词撤销屏蔽。
#[test]
fn english_user_word_purged_and_blocked_then_readd() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("user");
    store
        .add_user_word("english", "wxyzqk", "Wxyzqk", 0, 0)
        .unwrap();
    store
        .add_user_word("english", "wxyz", "Wxyzqk", 0, 0)
        .unwrap();
    assert!(
        has_dict_word(&triples(&cfg, &store, "wxyzq"), "Wxyzqk"),
        "前提"
    );
    delete_on_page(&cfg, &store, "wxyzq", "Wxyzqk");
    assert!(
        store
            .get_user_words("english", "wxyzqk")
            .unwrap()
            .is_empty()
    );
    assert!(
        store.get_user_words("english", "wxyz").unwrap().is_empty(),
        "所有编码都删"
    );
    assert_eq!(blocked(&store, "english"), vec!["Wxyzqk".to_string()]);
    // 被屏蔽的词计入批量的「被隐藏」计数，单条加词撤销屏蔽
    let same = |c: &str| vec![c.to_string()];
    assert_eq!(
        store
            .words_hidden_by_shadow("english", &[("wxyzqk", "Wxyzqk")], &same)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .clear_deleted_for_words("english", &[("wxyzqk", "Wxyzqk")], &same)
            .unwrap(),
        1
    );
    assert!(blocked(&store, "english").is_empty());
}

/// 屏蔽的词不自动学：造过的临时词删掉后，再回车上屏同样的原文不再学回。
#[test]
fn auto_learn_skips_blocked_word() {
    skip_without_data!();
    let mut cfg = config("english");
    cfg.schema.english.auto_learn.enabled = true;
    let store = fresh_store("learn");
    let coord = open(&cfg, &store);
    type_str(&coord, "wxyzqk");
    coord.handle_key_event_policed(&key(VK_RETURN, 0));
    let temp = |s: &Store| s.get_temp_word("english", "wxyzqk", "wxyzqk").unwrap();
    assert_eq!(temp(&store), Some(1), "前提：学进临时词库");
    delete_on_page(&cfg, &store, "wxyz", "wxyzqk");
    assert_eq!(temp(&store), None, "临时词真删");
    assert_eq!(blocked(&store, "english"), vec!["wxyzqk".to_string()]);
    let coord = open(&cfg, &store);
    type_str(&coord, "wxyzqk");
    match coord.handle_key_event_policed(&key(VK_RETURN, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), "wxyzqk"),
        other => panic!("回车照常上屏，实际 {other:?}"),
    }
    assert_eq!(temp(&store), None, "屏蔽的词不再学回");
}

/// 混在拼音里的英文：英文用户词删得掉（去 `english` 桶按编码真删，不屏蔽）；
/// 英文方案的屏蔽集不影响拼音里的英文。
#[test]
fn pinyin_merged_english_user_word_deletable() {
    skip_without_data!();
    let mut cfg = config("pinyin");
    cfg.schema.pinyin.english_merge.enable = true;
    let store = fresh_store("py_en");
    store
        .add_user_word("english", "wxyzqk", "Wxyzqk", 0, 0)
        .unwrap();
    assert!(
        has_dict_word(&triples(&cfg, &store, "wxyzqk"), "Wxyzqk"),
        "前提"
    );
    delete_on_page(&cfg, &store, "wxyzqk", "Wxyzqk");
    assert!(
        store
            .get_user_words("english", "wxyzqk")
            .unwrap()
            .is_empty(),
        "英文用户词应真删"
    );
    assert!(
        blocked(&store, "english").is_empty(),
        "混入的英文不按文字屏蔽"
    );
    assert!(!has_dict_word(&triples(&cfg, &store, "wxyzqk"), "Wxyzqk"));

    // 英文方案屏蔽的词在拼音的英文混入里照出（混入的英文按宿主桶各自隐藏）
    store.block_word("english", "hello").unwrap();
    assert!(
        has_dict_word(&triples(&cfg, &store, "hello"), "hello"),
        "拼音里的英文不读 english 屏蔽集"
    );
}

/// 混输里的英文用户词同样删得掉。
#[test]
fn mixed_english_user_word_deletable() {
    skip_without_data!();
    let mut cfg = config("wubi86_pinyin");
    cfg.schema.mix.enable_english = true;
    let store = fresh_store("mix_en");
    store
        .add_user_word("english", "wxyzqk", "Wxyzqk", 0, 0)
        .unwrap();
    assert!(
        has_dict_word(&triples(&cfg, &store, "wxyzqk"), "Wxyzqk"),
        "前提"
    );
    delete_on_page(&cfg, &store, "wxyzqk", "Wxyzqk");
    assert!(
        store
            .get_user_words("english", "wxyzqk")
            .unwrap()
            .is_empty()
    );
    assert!(!has_dict_word(&triples(&cfg, &store, "wxyzqk"), "Wxyzqk"));
}
