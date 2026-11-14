//! 字母换位纠错候选（`gogn` → 「工」）的界面表现：标记、首选保护、去重、自动上屏。
//!
//! 走协调器断言界面序（引擎位次不作数，协调器还会整体重排）。真实词库在 `build_dev/data`，
//! 缺失时跳过。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::{CandidateItem, UiCommand};

const MARK: &str = "*";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

fn config(schema: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec![schema.into()];
    c.schema.active = schema.into();
    c.input.default.chinese_mode = true;
    c.input.correction.rules.gn_ng = true; // 出厂关；用例是 gognping / hagn
    c
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn type_str(coord: &Coordinator, s: &str) -> KeyAction {
    let mut last = KeyAction::ClearComposition;
    for ch in s.chars() {
        last = coord.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
    }
    last
}

struct Shown {
    items: Vec<CandidateItem>,
    preedit: String,
}

/// 敲入 `input`，放行首显闸门后取首页候选与编码栏。
fn show_with(cfg: Config, input: &str) -> Shown {
    let (coord, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    type_str(&coord, input);
    Shown {
        items: page(&coord, &rx),
        preedit: coord.debug_preedit(),
    }
}

fn show(input: &str) -> Shown {
    show_with(config("pinyin"), input)
}

fn show_off(input: &str) -> Shown {
    let mut c = config("pinyin");
    c.input.correction.enabled = false;
    show_with(c, input)
}

fn page(coord: &Arc<Coordinator>, rx: &Receiver<UiCommand>) -> Vec<CandidateItem> {
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .expect("应下发候选")
}

fn texts(items: &[CandidateItem]) -> Vec<&str> {
    items.iter().map(|c| c.text.as_str()).collect()
}

/// 逐项逐字段比对用（`CandidateItem` 没有 `PartialEq`）。
fn dump(items: &[CandidateItem]) -> String {
    format!("{items:?}")
}

fn marked(items: &[CandidateItem]) -> Vec<&str> {
    texts(items)
        .into_iter()
        .filter(|t| t.starts_with(MARK))
        .collect()
}

#[test]
fn gognping_yields_marked_gongping() {
    if !has_data() {
        return;
    }
    let s = show("gognping");
    assert!(
        s.items.iter().any(|i| i.text == "*公平"),
        "{:?}",
        texts(&s.items)
    );
}

#[test]
fn preedit_keeps_typed_spelling_on_correction() {
    if !has_data() {
        return;
    }
    assert_eq!(show("gognping").preedit, "gogn'ping");
}

#[test]
fn correct_input_has_no_marker() {
    if !has_data() {
        return;
    }
    let on = show("gongping");
    assert!(marked(&on.items).is_empty(), "{:?}", texts(&on.items));
    assert_eq!(dump(&on.items), dump(&show_off("gongping").items));
}

#[test]
fn typing_prefix_zhon_has_no_marker() {
    if !has_data() {
        return;
    }
    let s = show("zhon");
    assert!(marked(&s.items).is_empty(), "{:?}", texts(&s.items));
}

/// 有完整的非纠错候选时，纠错候选权重再高也不占首选（D6）。
///
/// 夹具：自定义 `in=ian` 让 `din` 可纠成 `dian`，in/ing 模糊让它同时读作 `ding`（「定」是完整
/// 的非纠错候选）；用户词「点」(dian) 权重极高，纠错罚分之后仍压过「定」。
#[test]
fn corrected_never_takes_top_when_complete_exists() {
    if !has_data() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("wind_corr_top_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(wind_store::Store::open(dir.join("u.redb")).unwrap());
    store
        .add_user_word("pinyin", "dian", "点", 1_000_000_000, 0)
        .unwrap();
    let mut c = config("pinyin");
    c.input.correction.custom = vec!["in=ian".into()];
    c.schema.pinyin.fuzzy.enabled = true;
    c.schema.pinyin.fuzzy.in_ing = true;
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), store);
    type_str(&coord, "din");
    let all = coord.debug_all_candidate_texts();
    drop(coord);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(all.iter().any(|t| t == "点"), "{all:?}");
    assert_eq!(all.first().map(String::as_str), Some("定"), "{all:?}");
    // 其余相对顺序不变：「点」只让出首位，仍在其余候选前面。
    assert_eq!(all.get(1).map(String::as_str), Some("点"), "{all:?}");
}

/// 打字途中 `zhon` 既是 `zhong` 的前缀，也是它的纠错拼写：「中」两条路都得到，不带标记，
/// 文字与次序同关纠错时（留下的那条码可能是 `zhon`，词频照记 `zhong`）。
#[test]
fn same_text_from_both_paths_is_unmarked() {
    if !has_data() {
        return;
    }
    let on = show("zhon");
    assert!(marked(&on.items).is_empty(), "{:?}", texts(&on.items));
    assert_eq!(texts(&on.items), texts(&show_off("zhon").items));
}

#[test]
fn empty_marker_shows_plain_text() {
    if !has_data() {
        return;
    }
    let mut c = config("pinyin");
    c.input.correction.marker = String::new();
    let s = show_with(c, "gognping");
    assert!(
        s.items.iter().any(|i| i.text == "公平"),
        "{:?}",
        texts(&s.items)
    );
    assert!(marked(&s.items).is_empty());
}

/// 关纠错 = 改动前：`gogn` 本无「工」，打对的输入候选不受影响。
#[test]
fn correction_disabled_is_byte_identical() {
    if !has_data() {
        return;
    }
    let off = show_off("gognping");
    assert!(marked(&off.items).is_empty(), "{:?}", texts(&off.items));
    assert!(
        !texts(&off.items).contains(&"公平"),
        "{:?}",
        texts(&off.items)
    );
    for input in ["gongping", "zhongguo", "nihao", "xian"] {
        assert_eq!(
            dump(&show_off(input).items),
            dump(&show(input).items),
            "{input}"
        );
    }
}

/// 非纠错候选在首页的相对顺序与关纠错时相同。
fn assert_plain_order_kept(input: &str, on: &[CandidateItem], off: &[CandidateItem]) {
    let off_texts = texts(off);
    let plain_on: Vec<&str> = texts(on)
        .into_iter()
        .filter(|t| !t.starts_with(MARK) && off_texts.contains(t))
        .collect();
    let plain_off: Vec<&str> = off_texts
        .iter()
        .copied()
        .filter(|t| plain_on.contains(t))
        .collect();
    assert_eq!(plain_on, plain_off, "{input}: 非纠错候选相对顺序变了");
}

/// 简拼冲突类输入：首选与关纠错时相同；非纠错候选在首页的相对顺序不变。
///
/// `xign` 只断言后一半：本文件的 `config()` 打开了 `gn_ng`（出厂关），`xign` 纠成 `xing` 的
/// 「型」按 D6 可以压过简拼（简拼不算完整候选，与 `gogn` 出「工」同一条规则），首选必然变
/// ——这正是设计 §5.2 给 `gn_ng` 记的冲突示例；出厂配置下的首选见
/// [`xign_top_unchanged_with_factory_rules`]。
///
/// 且 `xign` 的首页全是 *型 一类纠错候选（`xing` 的整串字排在 `xi` 半截之前），首页上比
/// 相对顺序会空过；故它改比**全部候选**，并先断言交集非空（「习惯」等仍在）。被纠错候选
/// 挤出取数窗口尾部是允许的（Ruling 8），故不要求关纠错的候选全数在列。
#[test]
fn abbrev_inputs_unaffected_by_correction() {
    if !has_data() {
        return;
    }
    for input in ["xign", "nage", "dnu", "xai"] {
        let on = show(input);
        let off = show_off(input);
        if input != "xign" {
            assert_eq!(
                on.items[0].text,
                off.items[0].text,
                "{input}: on {:?} / off {:?}",
                texts(&on.items),
                texts(&off.items)
            );
        }
        if input == "xign" {
            let on_all = all_texts(config("pinyin"), input);
            let mut c = config("pinyin");
            c.input.correction.enabled = false;
            let off_all = all_texts(c, input);
            let kept: Vec<&String> = on_all.iter().filter(|t| off_all.contains(t)).collect();
            let expect: Vec<&String> = off_all.iter().filter(|t| on_all.contains(t)).collect();
            assert!(kept.iter().any(|t| *t == "习惯"), "{on_all:?}");
            assert_eq!(kept, expect, "{input}: 非纠错候选相对顺序变了");
        } else {
            assert_plain_order_kept(input, &on.items, &off.items);
        }
    }
}

/// 出厂规则（`gn_ng` 关）：`xign` 首选与关纠错时相同（「习惯」）。
#[test]
fn xign_top_unchanged_with_factory_rules() {
    if !has_data() {
        return;
    }
    let factory = || {
        let mut c = Config::default();
        c.schema.available = vec!["pinyin".into()];
        c.schema.active = "pinyin".into();
        c.input.default.chinese_mode = true;
        c
    };
    let on = show_with(factory(), "xign");
    let mut off_cfg = factory();
    off_cfg.input.correction.enabled = false;
    let off = show_with(off_cfg, "xign");
    assert_eq!(off.items[0].text, "习惯", "前提：{:?}", texts(&off.items));
    assert_eq!(
        on.items[0].text,
        off.items[0].text,
        "on {:?} / off {:?}",
        texts(&on.items),
        texts(&off.items)
    );
}

/// 敲入 `input` 后的全部候选文字（界面序，不含标记）。
fn all_texts(cfg: Config, input: &str) -> Vec<String> {
    let (coord, _rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    type_str(&coord, input);
    coord.debug_all_candidate_texts()
}

/// 混输满码唯一：五笔 `hagn` 只有「甗」一条，拼音侧多出的纠错候选（hagn→hang）不拦自动上屏。
#[test]
fn corrected_candidate_skipped_by_auto_commit_count() {
    if !has_data() {
        return;
    }
    // 前提：拼音侧确有纠错候选。
    let pinyin_hagn = show("hagn");
    assert!(
        !marked(&pinyin_hagn.items).is_empty(),
        "{:?}",
        texts(&pinyin_hagn.items)
    );
    let mut c = config("wubi86_pinyin");
    c.schema.codetable.auto_commit_at_full = true;
    let (coord, _rx) = Coordinator::new_headless_with_ui(c, Some(&data_dir()));
    let action = type_str(&coord, "hagn");
    assert!(
        matches!(&action, KeyAction::InsertText { text, .. } if text == "甗"),
        "{action:?} / {:?}",
        coord.debug_all_candidate_texts()
    );
}

#[test]
fn shuangpin_has_no_letter_correction() {
    if !has_data() {
        return;
    }
    // 小鹤 `gs` = gong；`go` 读作 g+uo，换位纠错不介入。
    for input in ["gsps", "gogn"] {
        let s = show_with(config("shuangpin"), input);
        assert!(
            marked(&s.items).is_empty(),
            "{input}: {:?}",
            texts(&s.items)
        );
    }
}

/// 内置误读表召回词库读不出的词并标正确拼音（文档站「纠错与提示」页与变更记录里的例子）。
#[test]
fn builtin_misread_table_recalls_tiaoyan() {
    if !has_data() {
        return;
    }
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.input.correction.hint.enabled = true;
    for (input, text, styled) in [
        ("tiaoyan", "调研", "调(diao)研"),
        ("zhire", "炽热", "炽(chi)热"),
        ("bangbu", "蚌埠", "蚌(beng)埠"),
    ] {
        let all = all_texts(c.clone(), input);
        let pos = all
            .iter()
            .position(|t| t == text)
            .unwrap_or_else(|| panic!("{input} 应召回「{text}」：{all:?}"));
        assert!(pos > 0, "召回候选不占首选：{all:?}");
        let shown = show_with(c.clone(), input);
        let item = shown
            .items
            .iter()
            .find(|i| i.text == text)
            .unwrap_or_else(|| panic!("「{text}」应在首页：{:?}", texts(&shown.items)));
        assert_eq!(item.text_styled.as_str(), styled);
    }
}
