//! 误读表（设计 `docs/design/pinyin-correction-and-hints.md` §6）：打表里的错误读音时召回词库
//! 读不出的词并内嵌正确拼音（「莘(shen)莘(shen)」），召回候选不占首选；词库已有同文字候选时
//! 不另召回、照标提示。
//!
//! 自造夹具：误读表写在 `schemas/pinyin_misread.yaml`（内置表位置）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyEventData, MessageHandler};
use wind_config::{Config, HintScope};
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::{CandidateItem, UiCommand};

const DICT: &str = "欣欣\txin xin\t5000\n心心\txin xin\t3000\n莘莘\tshen shen\t100\n\
                    馄饨\thun tun\t800\n馄饨\thun dun\t50\n混沌\thun dun\t900\n";

const TABLE: &str = "---\nname: t\n...\n\
                     - {text: 莘莘, wrong: xin xin, right: shen shen}\n\
                     - {text: 馄饨, wrong: hun dun, right: hun tun}\n";

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(tag: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("wind_misread_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("mr")).unwrap();
    std::fs::write(
        schemas.join("mr/mini.dict.yaml"),
        format!("---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n{DICT}"),
    )
    .unwrap();
    std::fs::write(
        schemas.join("mr.schema.toml"),
        "[schema]\nid = \"mr\"\nname = \"mr\"\n[engine]\ntype = \"pinyin\"\n\
         [[dictionaries]]\nid = \"mini\"\npath = \"mr/mini.dict.yaml\"\n\
         type = \"rime_pinyin\"\ndefault = true\n",
    )
    .unwrap();
    std::fs::write(schemas.join("pinyin_misread.yaml"), TABLE).unwrap();
    Fixture { dir }
}

fn open(f: &Fixture, misread: bool, hint: bool) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let mut c = Config::default();
    c.schema.available = vec!["mr".into()];
    c.schema.active = "mr".into();
    c.input.default.chinese_mode = true;
    let h = &mut c.input.correction.hint;
    h.enabled = hint;
    h.scope = HintScope::All;
    h.misread = misread;
    Coordinator::new_headless_with_ui(c, Some(&f.dir))
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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
    }
}

/// 放行首显闸门后取最近一次下发的候选页。
fn shown(coord: &Coordinator, rx: &Receiver<UiCommand>) -> Vec<CandidateItem> {
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

fn typed(tag: &str, misread: bool, input: &str) -> Vec<CandidateItem> {
    typed_with(tag, misread, true, input)
}

fn typed_with(tag: &str, misread: bool, hint: bool, input: &str) -> Vec<CandidateItem> {
    let f = fixture(tag);
    let (coord, rx) = open(&f, misread, hint);
    type_str(&coord, input);
    shown(&coord, &rx)
}

#[test]
fn misread_recall_shows_hinted_candidate() {
    let items = typed("recall", true, "xinxin");
    let pos = items
        .iter()
        .position(|c| c.text == "莘莘")
        .unwrap_or_else(|| panic!("应召回「莘莘」：{:?}", texts(&items)));
    assert_eq!(items[pos].text_styled.as_str(), "莘(shen)莘(shen)");
    assert!(pos > 0, "召回候选不占首选：{:?}", texts(&items));
}

#[test]
fn misread_recall_off_when_source_unchecked() {
    let items = typed("off", false, "xinxin");
    assert!(
        !items.iter().any(|c| c.text == "莘莘"),
        "{:?}",
        texts(&items)
    );
}

/// 词库已有同文字候选（错误读音收进了词库）：不另召回，那一条照标提示。
#[test]
fn dict_candidate_with_wrong_reading_gets_hint_once() {
    let items = typed("dedupe", true, "hundun");
    let hits: Vec<&CandidateItem> = items.iter().filter(|c| c.text == "馄饨").collect();
    assert_eq!(hits.len(), 1, "{:?}", texts(&items));
    assert_eq!(hits[0].text_styled.as_str(), "馄饨(tun)");
}

/// 召回只看「误读」来源勾选，不看「显示正确拼音」：关显示时照样召回，只是不内嵌提示。
#[test]
fn misread_recall_without_inline_hint() {
    let items = typed_with("nohint", true, false, "xinxin");
    let c = items
        .iter()
        .find(|c| c.text == "莘莘")
        .unwrap_or_else(|| panic!("应召回「莘莘」：{:?}", texts(&items)));
    assert!(c.text_styled.is_empty(), "{:?}", c.text_styled.as_str());
}

/// 简拼 / 混合简拼带出的词不按表标：用户没打出那个错误读音（`hd`、`hund` 只是缩写）。
#[test]
fn abbrev_candidate_is_not_flagged() {
    for input in ["hd", "hund"] {
        let items = typed(&format!("abbrev_{input}"), true, input);
        let c = items
            .iter()
            .find(|c| c.text == "馄饨")
            .unwrap_or_else(|| panic!("{input} 应有「馄饨」：{:?}", texts(&items)));
        assert!(
            c.text_styled.is_empty(),
            "{input}: {:?}",
            c.text_styled.as_str()
        );
    }
}
