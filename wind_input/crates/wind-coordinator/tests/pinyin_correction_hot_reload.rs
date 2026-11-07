//! `[input.correction]` 改了不用重启：经 `reload_user_config` 热重载即生效——「误读」勾选决定召回，
//! 纠错规则决定纠错候选。
//!
//! 重载从磁盘读配置，用宿主配置改写（`set_config_overlay`）把读到的整份换成用例给的配置，
//! 不受本机用户配置影响。改写是不带捕获的函数指针，每个场景各用一份静态配置。

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use wind_bridge::handler::{CaretData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::{CandidateItem, UiCommand};

const DICT: &str = "欣欣\txin xin\t5000\n心心\txin xin\t3000\n莘莘\tshen shen\t100\n\
                    公平\tgong ping\t3000\n";

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(tag: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("wind_corr_reload_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("cr")).unwrap();
    std::fs::write(
        schemas.join("cr/mini.dict.yaml"),
        format!("---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n{DICT}"),
    )
    .unwrap();
    std::fs::write(
        schemas.join("cr.schema.toml"),
        "[schema]\nid = \"cr\"\nname = \"cr\"\n[engine]\ntype = \"pinyin\"\n\
         [[dictionaries]]\nid = \"mini\"\npath = \"cr/mini.dict.yaml\"\n\
         type = \"rime_pinyin\"\ndefault = true\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("pinyin_misread.yaml"),
        "- {text: 莘莘, wrong: xin xin, right: shen shen}\n",
    )
    .unwrap();
    Fixture { dir }
}

/// 先规整：重载读盘时 `Config::load` 已规整过，用例给的配置也得是这个形态，否则方案段
/// 每次都「变了」、次次重建，就测不出纠错段自己的同步。
fn base_config(edit: impl FnOnce(&mut Config)) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["cr".into()];
    c.schema.active = "cr".into();
    c.input.default.chinese_mode = true;
    edit(&mut c);
    c.normalize();
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

const VK_ESCAPE: u32 = 0x1B;

/// 清掉上一次的输入，敲 `s`，放行首显闸门后取首页候选文字（去掉纠错标记）。
fn texts_for(coord: &Coordinator, rx: &Receiver<UiCommand>, s: &str) -> Vec<String> {
    coord.handle_key_event(&key(VK_ESCAPE));
    for ch in s.chars() {
        coord.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
    }
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    let items: Vec<CandidateItem> = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .expect("应下发候选");
    items
        .iter()
        .map(|c| c.text.trim_start_matches('*').to_string())
        .collect()
}

fn open(f: &Fixture, cfg: Config) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    Coordinator::new_headless_with_ui(cfg, Some(&f.dir))
}

static MISREAD_CFG: Mutex<Option<Config>> = Mutex::new(None);

fn misread_overlay(cfg: &mut Config) {
    if let Some(c) = MISREAD_CFG.lock().unwrap().clone() {
        *cfg = c;
    }
}

fn set_misread(on: bool) -> Config {
    let c = base_config(|c| c.input.correction.hint.misread = on);
    *MISREAD_CFG.lock().unwrap() = Some(c.clone());
    c
}

#[test]
fn misread_recall_follows_reload() {
    let f = fixture("misread");
    let (coord, rx) = open(&f, set_misread(true));
    assert!(coord.set_config_overlay(misread_overlay));
    let has = |coord: &Coordinator| texts_for(coord, &rx, "xinxin").contains(&"莘莘".to_string());
    assert!(has(&coord), "前提：出厂召回");

    set_misread(false);
    coord.reload_user_config();
    assert!(!has(&coord), "取消「误读」后不再召回");

    set_misread(true);
    coord.reload_user_config();
    assert!(has(&coord), "重新勾选后又召回");
}

static RULES_CFG: Mutex<Option<Config>> = Mutex::new(None);

fn rules_overlay(cfg: &mut Config) {
    if let Some(c) = RULES_CFG.lock().unwrap().clone() {
        *cfg = c;
    }
}

fn set_gn_ng(on: bool) -> Config {
    let c = base_config(|c| c.input.correction.rules.gn_ng = on);
    *RULES_CFG.lock().unwrap() = Some(c.clone());
    c
}

#[test]
fn correction_rule_follows_reload() {
    let f = fixture("rules");
    let (coord, rx) = open(&f, set_gn_ng(false));
    assert!(coord.set_config_overlay(rules_overlay));
    let has = |coord: &Coordinator| texts_for(coord, &rx, "gognping").contains(&"公平".to_string());
    assert!(!has(&coord), "前提：gn_ng 关时无「公平」");

    set_gn_ng(true);
    coord.reload_user_config();
    assert!(has(&coord), "开 gn_ng 后即出「公平」");
}
