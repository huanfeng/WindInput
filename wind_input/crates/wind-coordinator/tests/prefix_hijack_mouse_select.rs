//! 网址 / 邮箱 / Unicode 模式**鼠标点选**候选：与键盘空格走同一个出口。
//!
//! 现场：`select_candidate_at` 的模式派发里这三个模式落 `_ => None`，掉进下方通用分支
//! `commit_candidate`——拿主路 `input_buffer`（overlay 下恒空）当码记选词 ⇒ 拼音桶里写出
//! `("", "a@qq.com")` 这种空码孤儿词频；不经 `commit_email` / `commit_url` ⇒ 不记统计、
//! 不学后缀 / 网址历史。键盘空格上屏同一条候选则全对。
//!
//! 主方案取拼音：空码孤儿行落的正是活跃方案的词频桶。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;
use wind_store::completion::CompletionKind;
use wind_store::stats::CommitSource;

const VK_2: u32 = 0x32;
const VK_PERIOD: u32 = 0xBE;
const VK_PLUS: u32 = 0xBB;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir().join("schemas/pinyin.schema.toml").exists()
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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let k = if ch.is_ascii_digit() {
            ch as u32
        } else {
            (ch.to_ascii_uppercase() as u32) & 0xFF
        };
        coord.handle_key_event(&key(k, 0));
    }
}

fn open(tag: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.frequency.enabled = true;
    edit(&mut c);
    let db = std::env::temp_dir().join(format!(
        "wind_hijack_mouse_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

/// 点选当页上文本为 `want` 的候选，断言整体上屏、统计按 `source` 记一笔、不留词频行。
fn mouse_pick(coord: &Coordinator, store: &Store, want: &str, source: CommitSource) {
    let page = coord.debug_page_texts();
    let pos = page
        .iter()
        .position(|t| t == want)
        .unwrap_or_else(|| panic!("前提：候选页应有 {want}，实际: {page:?}"));
    coord.debug_capture_stat_events();
    match coord.debug_mouse_select(pos) {
        Some(KeyAction::InsertText { text, .. }) => assert_eq!(text, want, "前提：点选上屏"),
        other => panic!("点选应整体上屏，实际: {other:?}"),
    }
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(source, want.to_string())],
        "点选应与键盘空格同一出口记统计"
    );
    let (rows, _) = store.list_freq_paged("pinyin", "", 0, 0).unwrap();
    let orphans: Vec<_> = rows
        .iter()
        .filter(|(code, text, _)| code.is_empty() || text == want)
        .map(|(c, t, _)| (c.clone(), t.clone()))
        .collect();
    assert!(
        orphans.is_empty(),
        "点选不应写词频（空码孤儿行）: {orphans:?}"
    );
    assert_eq!(coord.debug_active_mode(), None, "上屏后应退出模式");
}

#[test]
fn email_mouse_select_uses_keyboard_exit() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("email", |c| c.input.email.enabled = true);
    type_str(&coord, "a");
    coord.handle_key_event(&key(VK_2, MOD_SHIFT)); // `@`
    mouse_pick(&coord, &store, "a@qq.com", CommitSource::Email);
    let (learned, _) = store
        .list_completions(CompletionKind::EmailSuffix, "", 0, 0)
        .unwrap();
    assert!(
        learned.iter().any(|(s, _)| s == "qq.com"),
        "点选应学后缀 qq.com，实际: {learned:?}"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn url_mouse_select_uses_keyboard_exit() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("url", |c| {
        c.input.url.enabled = true;
        c.input.url.prefixes = vec!["www.".into()];
        c.input.url.history_enabled = true;
    });
    store
        .record_completion(CompletionKind::UrlHistory, "www.example.com")
        .unwrap();
    type_str(&coord, "www");
    coord.handle_key_event(&key(VK_PERIOD, 0));
    type_str(&coord, "e");
    mouse_pick(&coord, &store, "www.example.com", CommitSource::Url);
    let (hist, _) = store
        .list_completions(CompletionKind::UrlHistory, "", 0, 0)
        .unwrap();
    let count = hist
        .iter()
        .find(|(t, _)| t == "www.example.com")
        .map(|(_, r)| r.count);
    assert_eq!(
        count,
        Some(2),
        "点选应记网址历史（次数 +1），实际: {hist:?}"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn unicode_mouse_select_uses_keyboard_exit() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("unicode", |c| c.input.unicode.enabled = true);
    type_str(&coord, "u");
    coord.handle_key_event(&key(VK_PLUS, MOD_SHIFT)); // `+`
    type_str(&coord, "4e2d");
    mouse_pick(&coord, &store, "中", CommitSource::RawInput);
    let _ = std::fs::remove_file(&db);
}
